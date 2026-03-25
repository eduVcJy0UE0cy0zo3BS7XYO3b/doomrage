;;; canvas-mode.el --- Emacs integration for wasm-canvas via nREPL -*- lexical-binding: t; -*-

;; Usage: M-x eval-buffer, then M-x canvas-connect
;; Or: (canvas-connect) in your init.el

(require 'cl-lib)

;;; --- nREPL transport (bencode over TCP) ---

(defvar canvas--process nil "Network process for nREPL connection.")
(defvar canvas--session nil "Current nREPL session ID.")
(defvar canvas--buffer "*canvas-repl*" "REPL output buffer.")
(defvar canvas--response-queue nil "Pending response accumulator.")
(defvar canvas--callbacks (make-hash-table :test 'equal) "Callbacks keyed by message ID.")
(defvar canvas--msg-counter 0 "Message ID counter.")
(defvar canvas--partial-data "" "Partial bencode data between filter calls.")
(defvar canvas--current-ns nil "Current namespace (canvas/node).")

;; --- Bencode ---

(defun canvas--bencode-encode (obj)
  "Encode OBJ as bencode string."
  (cond
   ((stringp obj) (format "%d:%s" (string-bytes obj) obj))
   ((integerp obj) (format "i%de" obj))
   ((vectorp obj)
    ;; Vector = list (use vectors to force list encoding)
    (concat "l"
            (mapconcat #'canvas--bencode-encode (append obj nil) "")
            "e"))
   ((listp obj)
    (if (and (consp (car obj)) (stringp (caar obj)))
        ;; Alist = dict
        (concat "d"
                (mapconcat (lambda (pair)
                             (concat (canvas--bencode-encode (car pair))
                                     (canvas--bencode-encode (cdr pair))))
                           (sort (copy-sequence obj)
                                 (lambda (a b) (string< (car a) (car b))))
                           "")
                "e")
      ;; List
      (concat "l"
              (mapconcat #'canvas--bencode-encode obj "")
              "e")))
   (t "")))

(defun canvas--bencode-decode-from (str pos)
  "Decode bencode value from STR starting at POS. Return (value . new-pos)."
  (when (>= pos (length str))
    (signal 'canvas-incomplete-bencode nil))
  (let ((ch (aref str pos)))
    (cond
     ;; Integer
     ((= ch ?i)
      (let ((end (cl-position ?e str :start (1+ pos))))
        (unless end (signal 'canvas-incomplete-bencode nil))
        (cons (string-to-number (substring str (1+ pos) end))
              (1+ end))))
     ;; List
     ((= ch ?l)
      (let ((items nil) (p (1+ pos)))
        (while (and (< p (length str)) (/= (aref str p) ?e))
          (let ((result (canvas--bencode-decode-from str p)))
            (push (car result) items)
            (setq p (cdr result))))
        (when (>= p (length str)) (signal 'canvas-incomplete-bencode nil))
        (cons (nreverse items) (1+ p))))
     ;; Dict
     ((= ch ?d)
      (let ((dict nil) (p (1+ pos)))
        (while (and (< p (length str)) (/= (aref str p) ?e))
          (let* ((key-result (canvas--bencode-decode-from str p))
                 (val-result (canvas--bencode-decode-from str (cdr key-result))))
            (push (cons (car key-result) (car val-result)) dict)
            (setq p (cdr val-result))))
        (when (>= p (length str)) (signal 'canvas-incomplete-bencode nil))
        (cons (nreverse dict) (1+ p))))
     ;; String
     ((and (>= ch ?0) (<= ch ?9))
      (let* ((colon (cl-position ?: str :start pos))
             (_ (unless colon (signal 'canvas-incomplete-bencode nil)))
             (len (string-to-number (substring str pos colon)))
             (start (1+ colon))
             (end (+ start len)))
        (when (> end (length str)) (signal 'canvas-incomplete-bencode nil))
        (cons (substring str start end) end)))
     (t (error "Unexpected bencode byte: %c at %d" ch pos)))))

(defun canvas--bencode-decode (str)
  "Decode first bencode value from STR. Return (value . remaining-str)."
  (condition-case nil
      (let ((result (canvas--bencode-decode-from str 0)))
        (cons (car result) (substring str (cdr result))))
    (canvas-incomplete-bencode (cons nil str))))

(define-error 'canvas-incomplete-bencode "Incomplete bencode data")

;; --- Path helpers ---

(defun canvas--infer-from-path ()
  "Infer canvas and label from current buffer file path.
Returns (canvas . label) or nil if not a .canvas/nodes/ file."
  (when buffer-file-name
    (let ((path (expand-file-name buffer-file-name)))
      (when (string-match "/\\.?canvas/nodes/\\([^/]+\\)/\\([^/]+\\)\\.scm\\'" path)
        (cons (match-string 1 path)
              (match-string 2 path))))))

(defun canvas--ensure-ns ()
  "Ensure canvas--current-ns is set. Infer from file path if needed."
  (unless canvas--current-ns
    (when-let ((info (canvas--infer-from-path)))
      (setq canvas--current-ns (format "%s/%s" (car info) (cdr info)))))
  canvas--current-ns)

(defun canvas--ensure-connected ()
  "Auto-connect if not connected and port file exists."
  (unless canvas--session
    (when-let ((port (canvas--port)))
      (canvas-connect port)))
  (unless canvas--session
    (error "Not connected. Run M-x canvas-connect")))

(defun canvas--extract-defines ()
  "Extract all top-level define names from the current buffer."
  (save-excursion
    (goto-char (point-min))
    (let (names)
      (while (re-search-forward "^(define\\(?:-record-type\\|-syntax\\)?\\s-+(?\\(\\S-+\\)" nil t)
        (push (match-string 1) names))
      (nreverse names))))

;; --- Connection ---

(defun canvas--port ()
  "Read nREPL port from ~/.canvas/.nrepl-port."
  (let ((file (expand-file-name "~/.canvas/.nrepl-port")))
    (when (file-exists-p file)
      (string-to-number (string-trim (with-temp-buffer
                                       (insert-file-contents file)
                                       (buffer-string)))))))

(defun canvas--next-id ()
  "Generate next message ID."
  (setq canvas--msg-counter (1+ canvas--msg-counter))
  (format "emacs-%d" canvas--msg-counter))

(defun canvas--send (msg)
  "Send bencode MSG dict to nREPL server."
  (when canvas--process
    (process-send-string canvas--process (canvas--bencode-encode msg))))

(defun canvas--send-op (op &rest pairs)
  "Send an nREPL operation. PAIRS are extra key-value pairs."
  (let* ((id (canvas--next-id))
         (msg `(("id" . ,id)
                ("op" . ,op)
                ,@(when canvas--session `(("session" . ,canvas--session)))
                ,@(cl-loop for (k v) on pairs by #'cddr
                           collect (cons k v)))))
    (canvas--send msg)
    id))

(defun canvas--filter (proc str)
  "Network filter: accumulate bencode data, dispatch complete messages."
  (setq canvas--partial-data (concat canvas--partial-data str))
  (let ((continue t))
    (while continue
      (let ((result (canvas--bencode-decode canvas--partial-data)))
        (if (car result)
            (progn
              (setq canvas--partial-data (cdr result))
              (canvas--handle-response (car result)))
          (setq continue nil))))))

(defun canvas--handle-response (resp)
  "Handle a decoded nREPL response dict."
  ;; Capture session from clone response
  (when-let ((new-session (cdr (assoc "new-session" resp))))
    (setq canvas--session new-session))
  (let ((id (cdr (assoc "id" resp)))
        (cb nil))
    ;; Check for registered callback
    (when (and id (setq cb (gethash id canvas--callbacks)))
      (let ((status (cdr (assoc "status" resp))))
        (funcall cb resp)
        ;; Remove callback if "done"
        (when (and (listp status) (member "done" status))
          (remhash id canvas--callbacks))))
    ;; Print to REPL buffer (skip clone/internal responses)
    (unless (cdr (assoc "new-session" resp))
      (canvas--print-response resp))))

(defun canvas--print-response (resp)
  "Print nREPL response to REPL buffer."
  (with-current-buffer (get-buffer-create canvas--buffer)
    (goto-char (point-max))
    (when-let ((out (cdr (assoc "out" resp))))
      (insert out))
    (when-let ((err (cdr (assoc "err" resp))))
      (insert (propertize err 'face 'error)))
    (when-let ((val (cdr (assoc "value" resp))))
      (insert (propertize val 'face 'font-lock-string-face) "\n"))
    (when-let ((ex (cdr (assoc "ex" resp))))
      (insert (propertize (concat "Error: " ex) 'face 'error) "\n"))))

(defun canvas--sync-request (op &rest pairs)
  "Send OP synchronously, wait for done, return all responses."
  (let* ((responses nil)
         (done nil)
         (id (apply #'canvas--send-op op pairs)))
    (puthash id
             (lambda (resp)
               (push resp responses)
               (let ((status (cdr (assoc "status" resp))))
                 (when (and (listp status) (member "done" status))
                   (setq done t))))
             canvas--callbacks)
    ;; Wait with timeout
    (let ((deadline (+ (float-time) 5.0)))
      (while (and (not done) (< (float-time) deadline))
        (accept-process-output canvas--process 0.1)))
    (nreverse responses)))

;; --- Public commands ---

;;;###autoload
(defun canvas-connect (&optional port)
  "Connect to wasm-canvas nREPL server."
  (interactive)
  (let ((port (or port (canvas--port) 7888)))
    (when canvas--process
      (delete-process canvas--process))
    (setq canvas--partial-data "")
    (setq canvas--process
          (make-network-process
           :name "canvas-nrepl"
           :host "127.0.0.1"
           :service port
           :filter #'canvas--filter
           :coding 'binary
           :nowait nil))
    ;; Clone session
    (let* ((id (canvas--next-id))
           (msg `(("id" . ,id) ("op" . "clone"))))
      (canvas--send msg)
      ;; Wait for session
      (let ((deadline (+ (float-time) 3.0)))
        (while (and (not canvas--session) (< (float-time) deadline))
          (accept-process-output canvas--process 0.1))))
    ;; The clone response sets session via filter
    ;; If not set yet, parse from partial data
    (unless canvas--session
      (let ((result (canvas--bencode-decode canvas--partial-data)))
        (when (car result)
          (setq canvas--partial-data (cdr result))
          (setq canvas--session (cdr (assoc "new-session" (car result)))))))
    (if canvas--session
        (progn
          (message "Connected to canvas nREPL on port %d (session: %s)" port canvas--session)
          (pop-to-buffer canvas--buffer))
      (error "Failed to create nREPL session"))))

;;;###autoload
(defun canvas-eval (code)
  "Eval CODE in the canvas nREPL session."
  (interactive "sScheme: ")
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (canvas--send-op "eval" "code" code))

;;;###autoload
(defun canvas-eval-last-sexp ()
  "Eval the sexp before point."
  (interactive)
  (let ((end (point))
        (beg (save-excursion (backward-sexp) (point))))
    (canvas-eval (buffer-substring-no-properties beg end))))

;;;###autoload
(defun canvas-eval-region (beg end)
  "Eval the region."
  (interactive "r")
  (canvas-eval (buffer-substring-no-properties beg end)))

;;;###autoload
(defun canvas-eval-buffer ()
  "Eval the entire buffer."
  (interactive)
  (canvas-eval (buffer-substring-no-properties (point-min) (point-max))))

;;;###autoload
(defun canvas-load-file ()
  "Load the current .scm file into wasm-canvas."
  (interactive)
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let ((path (buffer-file-name))
        (content (buffer-substring-no-properties (point-min) (point-max))))
    (unless path (error "Buffer has no file"))
    (canvas--send-op "load-file" "file" content "file-path" path)))

;;;###autoload
(defun canvas-switch-ns ()
  "Switch to a node namespace (interactive selection)."
  (interactive)
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let* ((responses (canvas--sync-request "ns-list"))
         (last-resp (car (last responses)))
         (ns-list (cdr (assoc "ns-list" last-resp))))
    (if (not ns-list)
        (message "No namespaces available")
      (let* ((ns (completing-read "Node: " ns-list nil t))
             (parts (split-string ns "/"))
             (canvas (car parts))
             (label (cadr parts))
             (file (expand-file-name
                    (format "~/.canvas/nodes/%s/%s.scm" canvas label))))
        (canvas--send-op "switch-ns" "ns" ns)
        (setq canvas--current-ns ns)
        (if (file-exists-p file)
            (find-file file)
          (message "Switched to %s (file %s not found)" ns file))))))

;;;###autoload
(defun canvas-completions-at-point ()
  "Completion-at-point function for canvas Scheme."
  (let* ((end (point))
         (beg (save-excursion
                (skip-syntax-backward "w_-")
                (point)))
         (prefix (buffer-substring-no-properties beg end)))
    (when (and canvas--session (> (length prefix) 0))
      (let* ((responses (canvas--sync-request "completions" "prefix" prefix))
             (last-resp (car (last responses)))
             (completions (cdr (assoc "completions" last-resp)))
             (candidates (mapcar (lambda (c) (cdr (assoc "candidate" c)))
                                 completions)))
        (when candidates
          (list beg end candidates))))))

;;;###autoload
(defun canvas-info-at-point ()
  "Show info about symbol at point (go-to-definition with prefix arg)."
  (interactive)
  (canvas--ensure-connected)
  (canvas--ensure-ns)
  (let* ((symbol (thing-at-point 'symbol t))
         (responses (apply #'canvas--sync-request "info" "symbol" (or symbol "")
                          (when canvas--current-ns (list "ns" canvas--current-ns))))
         (resp (car (last responses)))
         (name (cdr (assoc "name" resp)))
         (ns (cdr (assoc "ns" resp)))
         (file (cdr (assoc "file" resp)))
         (line (cdr (assoc "line" resp)))
         (hash (cdr (assoc "hash" resp)))
         (doc (cdr (assoc "doc" resp))))
    (if (not name)
        (message "No info for '%s'" symbol)
      (if (and current-prefix-arg file (file-exists-p file))
          ;; With prefix arg: jump to definition
          (progn
            (find-file file)
            (when line (goto-char (point-min)) (forward-line (1- line))))
        ;; Without: show info in minibuffer
        (message "%s%s%s%s"
                 (if ns (format "[%s] " ns) "")
                 name
                 (if hash (format " #%s" hash) "")
                 (if doc (format " -- %s" doc) ""))))))

;;;###autoload
(defun canvas-create-node (canvas label)
  "Create a new Script node and open its .scm file."
  (interactive (list (read-string "Canvas: " "default")
                     (read-string "Node label: ")))
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let ((resp (car (last (canvas--sync-request "create-node"
                                               "canvas" canvas
                                               "label" label
                                               "code" ""
                                               "exports" '()
                                               )))))
    (if (not (canvas--ok-p resp))
        (message "Error: %s" (cdr (assoc "ex" resp)))
      (let ((file (expand-file-name
                   (format "~/.canvas/nodes/%s/%s.scm" canvas (replace-regexp-in-string " " "-" label)))))
        (find-file file)
        (canvas--send-op "switch-ns" "ns" (format "%s/%s" canvas (replace-regexp-in-string " " "-" label)))
        (setq canvas--current-ns (format "%s/%s" canvas (replace-regexp-in-string " " "-" label)))
        (message "Created node '%s' on canvas '%s'" label canvas)))))

(defun canvas--ok-p (resp)
  (let ((s (cdr (assoc "status" resp))))
    (and (listp s) (member "done" s) (not (member "error" s)))))

;;;###autoload
(defun canvas-delete-node (canvas label)
  "Delete a node from the canvas."
  (interactive
   (let ((info (canvas--infer-from-path)))
     (if info
         (list (car info) (cdr info))
       (list (read-string "Canvas: " "default")
             (read-string "Node label: ")))))
  (canvas--ensure-connected)
  (when (yes-or-no-p (format "Delete node '%s' from '%s'?" label canvas))
    (let ((resp (car (last (canvas--sync-request "delete-node" "canvas" canvas "label" label)))))
      (if (canvas--ok-p resp)
          (message "Deleted node '%s'" label)
        (message "Error: %s" (cdr (assoc "ex" resp)))))))

;;;###autoload
(defun canvas-set-exports (exports-str)
  "Set exports for the current node.
With no prefix arg: auto-detect all defines from buffer.
With prefix arg: prompt for space-separated names."
  (interactive
   (list (if current-prefix-arg
             (read-string "Exports (space-separated): ")
           (let ((detected (canvas--extract-defines)))
             (if detected
                 (mapconcat #'identity detected " ")
               (read-string "Exports (space-separated): "))))))
  (canvas--ensure-connected)
  (canvas--ensure-ns)
  (unless canvas--current-ns (error "Cannot determine node"))
  (let* ((parts (split-string canvas--current-ns "/"))
         (canvas (car parts))
         (label (cadr parts))
         (exports (split-string exports-str)))
    (let ((resp (car (last (canvas--sync-request "update-node"
                                                 "canvas" canvas
                                                 "label" label
                                                 "exports" exports)))))
      (if (canvas--ok-p resp)
          (message "Exports: %s" exports-str)
        (message "Error: %s" (cdr (assoc "ex" resp)))))))

;;;###autoload
(defun canvas-compute ()
  "Save, load, and compute the current node. Auto-detects canvas/label from file path."
  (interactive)
  (canvas--ensure-connected)
  (canvas--ensure-ns)
  (unless canvas--current-ns (error "Cannot determine node. Open a .scm file or M-x canvas-switch-ns"))
  (let* ((parts (split-string canvas--current-ns "/"))
         (canvas (car parts))
         (label (cadr parts)))
    ;; Save + load if buffer has unsaved changes
    (when (and buffer-file-name (buffer-modified-p))
      (save-buffer))
    (when buffer-file-name
      (canvas-load-file))
    (let ((resp (car (last (canvas--sync-request "compute" "canvas" canvas "label" label)))))
      (if (not (canvas--ok-p resp))
          (message "Error: %s" (cdr (assoc "ex" resp)))
        (message "Computing '%s'..." label)
        ;; Poll for result and show error/output
        (run-at-time 1.5 nil
                     (lambda ()
                       (when canvas--current-ns
                         (let* ((parts (split-string canvas--current-ns "/"))
                                (state (car (last (canvas--sync-request "node-state"
                                                                        "canvas" (car parts)
                                                                        "label" (cadr parts))))))
                           (when (canvas--ok-p state)
                             (if-let ((err (cdr (assoc "error" state))))
                                 (message "Error: %s" err)
                               (let ((outputs (cdr (assoc "outputs" state))))
                                 (message "Done: %s"
                                          (mapconcat (lambda (p) (format "%s=%s" (car p) (cdr p)))
                                                     outputs ", ")))))))))))))

;;;###autoload
(defun canvas-node-state ()
  "Show the current node's state: exports, imports, outputs, errors."
  (interactive)
  (canvas--ensure-connected)
  (canvas--ensure-ns)
  (unless canvas--current-ns (error "Cannot determine node"))
  (let* ((parts (split-string canvas--current-ns "/"))
         (canvas (car parts))
         (label (cadr parts))
         (resp (car (last (canvas--sync-request "node-state" "canvas" canvas "label" label)))))
    (if (not (canvas--ok-p resp))
        (message "Error: %s" (cdr (assoc "ex" resp)))
      (with-current-buffer (get-buffer-create "*canvas-node*")
        (let ((inhibit-read-only t))
          (erase-buffer)
          (insert (format "Node: %s/%s\n\n" canvas label))
          ;; Error
          (when-let ((err (cdr (assoc "error" resp))))
            (insert (propertize (format "ERROR: %s\n\n" err) 'face 'error)))
          ;; Exports
          (let ((exports (cdr (assoc "exports" resp))))
            (insert (format "Exports: %s\n" (if exports (mapconcat #'identity exports " ") "(none)"))))
          ;; Hash imports
          (let ((his (cdr (assoc "hash-imports" resp))))
            (insert (format "Hash imports: %d\n" (length his)))
            (dolist (hi his)
              (insert (format "  %s <- #%s\n"
                              (cdr (assoc "name" hi))
                              (substring (cdr (assoc "hash" hi)) 0 12)))))
          ;; Legacy imports
          (let ((imports (cdr (assoc "imports" resp))))
            (when (> (length imports) 0)
              (insert (format "Legacy imports: %d\n" (length imports)))
              (dolist (imp imports)
                (insert (format "  (%s %s)\n" (car imp) (cadr imp))))))
          ;; Outputs
          (let ((outputs (cdr (assoc "outputs" resp))))
            (insert "\nOutputs:\n")
            (if outputs
                (dolist (pair outputs)
                  (insert (format "  %s = %s\n" (car pair) (cdr pair))))
              (insert "  (none)\n"))))
        (goto-char (point-min))
        (special-mode)
        (display-buffer (current-buffer))))))

;;;###autoload
(defun canvas-add-import ()
  "Interactively add a hash import: pick from available definitions."
  (interactive)
  (canvas--ensure-connected)
  (canvas--ensure-ns)
  (unless canvas--current-ns (error "Cannot determine node"))
  (let* ((parts (split-string canvas--current-ns "/"))
         (canvas (car parts))
         (label (cadr parts))
         ;; Get all defs
         (defs-resp (car (last (canvas--sync-request "defs" "canvas" canvas))))
         (defs (cdr (assoc "defs" defs-resp))))
    (if (or (null defs) (= (length defs) 0))
        (message "No definitions available on canvas '%s'" canvas)
      ;; Build completion candidates: "name (from node) #hash"
      (let* ((candidates (mapcar (lambda (d)
                                   (let ((name (cdr (assoc "name" d)))
                                         (node (cdr (assoc "node" d)))
                                         (hash (cdr (assoc "hash" d))))
                                     (cons (format "%s  (from %s)  #%s" name node (substring hash 0 12))
                                           d)))
                                 defs))
             (choice (completing-read "Import definition: " candidates nil t))
             (entry (cdr (assoc choice candidates)))
             (hash (cdr (assoc "hash" entry)))
             (name (cdr (assoc "name" entry)))
             (local-name (read-string "Local name: " name)))
        (let ((resp (car (last (canvas--sync-request "add-hash-import"
                                                     "canvas" canvas
                                                     "label" label
                                                     "hash" hash
                                                     "local-name" local-name)))))
          (if (canvas--ok-p resp)
              (progn
                (message "Imported '%s' as '%s'" name local-name)
                (when (and buffer-file-name (file-exists-p buffer-file-name))
                  (revert-buffer t t t)))
            (message "Error: %s" (cdr (assoc "ex" resp)))))))))

;;;###autoload
(defun canvas-repl ()
  "Open interactive REPL prompt in minibuffer."
  (interactive)
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let ((code (read-string (format "canvas%s> "
                                   (if canvas--current-ns
                                       (concat ":" canvas--current-ns)
                                     "")))))
    (when (> (length code) 0)
      (with-current-buffer (get-buffer-create canvas--buffer)
        (goto-char (point-max))
        (insert (propertize (concat "> " code "\n") 'face 'font-lock-comment-face)))
      (canvas-eval code))))

;;;###autoload
(defun canvas-list-defs (canvas)
  "List all content-addressed definitions for CANVAS."
  (interactive (list (or (car (canvas--infer-from-path))
                         (read-string "Canvas: " "default"))))
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let* ((responses (canvas--sync-request "defs" "canvas" canvas))
         (resp (car (last responses)))
         (defs (cdr (assoc "defs" resp))))
    (if (or (null defs) (= (length defs) 0))
        (message "No definitions for canvas '%s'" canvas)
      (with-current-buffer (get-buffer-create "*canvas-defs*")
        (let ((inhibit-read-only t))
          (erase-buffer)
          (insert (format "Definitions for canvas '%s':\n\n" canvas))
          (insert (format "%-20s %-18s %-15s %s\n" "NAME" "HASH" "NODE" "FORM"))
          (insert (make-string 70 ?-) "\n")
          (dolist (def defs)
            (let ((name (cdr (assoc "name" def)))
                  (hash (cdr (assoc "hash" def)))
                  (node (cdr (assoc "node" def)))
                  (form (cdr (assoc "form" def))))
              (insert (format "%-20s %-18s %-15s %s\n" name hash node form)))))
        (goto-char (point-min))
        (special-mode)
        (display-buffer (current-buffer))))))

;;;###autoload
(defun canvas-def-source (hash)
  "Show canonical source of a definition by its content HASH."
  (interactive "sHash: ")
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let* ((responses (canvas--sync-request "def-source" "hash" hash))
         (resp (car (last responses)))
         (source (cdr (assoc "source" resp))))
    (if source
        (with-current-buffer (get-buffer-create (format "*def:%s*" (substring hash 0 (min 8 (length hash)))))
          (let ((inhibit-read-only t))
            (erase-buffer)
            (insert source))
          (scheme-mode)
          (goto-char (point-min))
          (display-buffer (current-buffer)))
      (message "No source found for hash %s" hash))))

;;;###autoload
(defun canvas-def-history (name canvas)
  "Show version history of definition NAME on CANVAS."
  (interactive (list (read-string "Definition name: " (thing-at-point 'symbol t))
                     (or (car (canvas--infer-from-path))
                         (read-string "Canvas: " "default"))))
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let* ((responses (canvas--sync-request "def-history" "name" name "canvas" canvas))
         (resp (car (last responses)))
         (history (cdr (assoc "history" resp))))
    (if (or (null history) (= (length history) 0))
        (message "No history for '%s'" name)
      (with-current-buffer (get-buffer-create "*canvas-history*")
        (let ((inhibit-read-only t))
          (erase-buffer)
          (insert (format "History of '%s' on canvas '%s':\n\n" name canvas))
          (insert (format "%-4s %-18s %-15s %s\n" "#" "HASH" "NODE" "FORM"))
          (insert (make-string 55 ?-) "\n")
          (let ((i 0))
            (dolist (entry history)
              (let ((hash (cdr (assoc "hash" entry)))
                    (node (cdr (assoc "node" entry)))
                    (form (cdr (assoc "form" entry))))
                (insert (format "%-4d %-18s %-15s %s\n"
                                i hash node form))
                (setq i (1+ i))))))
        (goto-char (point-min))
        (special-mode)
        (display-buffer (current-buffer))))))

;;;###autoload
(defun canvas-def-diff (hash-a hash-b)
  "Show structural diff between two definitions by their content hashes."
  (interactive "sHash A: \nsHash B: ")
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let* ((responses (canvas--sync-request "def-diff" "hash-a" hash-a "hash-b" hash-b))
         (resp (car (last responses)))
         (diff (cdr (assoc "diff" resp))))
    (if diff
        (with-current-buffer (get-buffer-create "*canvas-diff*")
          (let ((inhibit-read-only t))
            (erase-buffer)
            (insert (format "Diff: %s..  %s..\n\n"
                            (substring hash-a 0 (min 12 (length hash-a)))
                            (substring hash-b 0 (min 12 (length hash-b)))))
            (insert diff)
            ;; Colorize diff
            (goto-char (point-min))
            (while (re-search-forward "^\\(- .*\\)$" nil t)
              (put-text-property (match-beginning 1) (match-end 1) 'face '(:foreground "red")))
            (goto-char (point-min))
            (while (re-search-forward "^\\(\\+ .*\\)$" nil t)
              (put-text-property (match-beginning 1) (match-end 1) 'face '(:foreground "green"))))
          (goto-char (point-min))
          (special-mode)
          (display-buffer (current-buffer)))
      (message "Could not diff (missing source for one or both hashes)"))))

;;;###autoload
(defun canvas-migrate-imports (canvas label)
  "Migrate legacy module imports to hash-based imports for current node."
  (interactive (let ((info (canvas--infer-from-path)))
                 (if info
                     (list (car info) (cdr info))
                   (list (read-string "Canvas: " "default")
                         (read-string "Node label: ")))))
  (canvas--ensure-connected)
  (let* ((responses (canvas--sync-request "migrate-imports" "canvas" canvas "label" label))
         (resp (car (last responses)))
         (migrated (cdr (assoc "migrated" resp)))
         (status (cdr (assoc "status" resp))))
    (if (member "error" status)
        (message "Error: %s" (cdr (assoc "ex" resp)))
      (message "Migrated %d imports to hash-based" (length migrated)))))

;;;###autoload
(defun canvas-rename-def (old-name new-name canvas)
  "Rename a definition across the canvas.
Updates source code, exports, Name DB, and all hash_imports in consumers."
  (interactive
   (let* ((sym (thing-at-point 'symbol t))
          (old (read-string "Rename: " sym))
          (new (read-string (format "'%s' → " old)))
          (canvas (or (car (canvas--infer-from-path)) (read-string "Canvas: " "default"))))
     (list old new canvas)))
  (canvas--ensure-connected)
  (let* ((responses (canvas--sync-request "rename-def"
                                          "canvas" canvas
                                          "old-name" old-name
                                          "new-name" new-name))
         (resp (car (last responses)))
         (status (cdr (assoc "status" resp))))
    (if (member "error" status)
        (message "Error: %s" (cdr (assoc "ex" resp)))
      (let ((updated (cdr (assoc "updated" resp))))
        (message "Renamed '%s' → '%s' (%s nodes updated)" old-name new-name updated)
        ;; Revert buffer if visiting a .scm file that may have changed
        (when buffer-file-name
          (revert-buffer t t t))))))

;;;###autoload
(defun canvas-add-hash-import (canvas label hash local-name)
  "Add a hash-based import to a node.
Import definition HASH as LOCAL-NAME into node LABEL on CANVAS."
  (interactive
   (let* ((canvas (read-string "Canvas: " "main"))
          (label (read-string "Node label: "))
          (hash (read-string "Definition hash: "))
          (local-name (read-string "Local name: ")))
     (list canvas label hash local-name)))
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let* ((responses (canvas--sync-request "add-hash-import"
                                          "canvas" canvas
                                          "label" label
                                          "hash" hash
                                          "local-name" local-name))
         (resp (car (last responses)))
         (status (cdr (assoc "status" resp))))
    (if (member "error" status)
        (message "Error: %s" (cdr (assoc "ex" resp)))
      (message "Added hash import: %s -> %s" (substring hash 0 (min 16 (length hash))) local-name))))

;; Pretty-print hash import pragmas: resolve hashes to human-readable names
(defun canvas--prettify-hash-imports ()
  "Add overlays to ;;; @import lines showing resolved names."
  (when (and canvas--session buffer-file-name)
    (save-excursion
      (goto-char (point-min))
      (while (re-search-forward "^;;; @import \\([0-9a-f]+\\) \\(\\S-+\\)" nil t)
        (let* ((hash (match-string 1))
               (local-name (match-string 2))
               (responses (ignore-errors
                            (canvas--sync-request "def-source" "hash" hash)))
               (resp (and responses (car (last responses))))
               (source (and resp (cdr (assoc "source" resp))))
               (ov (make-overlay (match-beginning 0) (match-end 0))))
          (overlay-put ov 'canvas-hash-import t)
          (when source
            (overlay-put ov 'after-string
                         (propertize (format "  ; %s" (truncate-string-to-width source 40))
                                     'face 'font-lock-comment-face))))))))

(defun canvas--clear-hash-import-overlays ()
  "Remove hash import overlays."
  (remove-overlays (point-min) (point-max) 'canvas-hash-import t))

;; --- Minor mode ---

(defvar canvas-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-e") #'canvas-eval-last-sexp)
    (define-key map (kbd "C-c C-r") #'canvas-eval-region)
    (define-key map (kbd "C-c C-b") #'canvas-eval-buffer)
    (define-key map (kbd "C-c C-l") #'canvas-load-file)
    (define-key map (kbd "C-c C-n") #'canvas-switch-ns)
    (define-key map (kbd "C-c C-d") #'canvas-info-at-point)
    (define-key map (kbd "C-c C-z") #'canvas-repl)
    (define-key map (kbd "C-c C-c") #'canvas-compute)
    (define-key map (kbd "C-c C-s") #'canvas-node-state)
    (define-key map (kbd "C-c C-x e") #'canvas-set-exports)
    (define-key map (kbd "C-c C-x i") #'canvas-add-import)
    (define-key map (kbd "C-c C-x r") #'canvas-rename-def)
    (define-key map (kbd "C-c C-x m") #'canvas-migrate-imports)
    (define-key map (kbd "C-c C-x d") #'canvas-list-defs)
    (define-key map (kbd "C-c C-x h") #'canvas-def-history)
    map))

;;;###autoload
(define-minor-mode canvas-mode
  "Minor mode for editing wasm-canvas .scm files with nREPL integration."
  :lighter " Canvas"
  :keymap canvas-mode-map
  (if canvas-mode
      (progn
        (add-hook 'completion-at-point-functions #'canvas-completions-at-point nil t)
        ;; Auto-connect
        (unless canvas--session
          (when (canvas--port)
            (condition-case err
                (canvas-connect)
              (error (message "Canvas: %s" (error-message-string err))))))
        ;; Auto-set namespace from file path
        (canvas--ensure-ns)
        (canvas--prettify-hash-imports))
    (canvas--clear-hash-import-overlays)
    (remove-hook 'completion-at-point-functions #'canvas-completions-at-point t)))

;; Auto-activate for .scm files under ~/.canvas/nodes/
(add-hook 'scheme-mode-hook
          (lambda ()
            (when (and buffer-file-name
                       (string-match-p "\\.canvas/nodes/" buffer-file-name))
              (canvas-mode 1))))

(provide 'canvas-mode)
;;; canvas-mode.el ends here
