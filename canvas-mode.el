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
  (unless canvas--session (error "Not connected. Run M-x canvas-connect"))
  (let* ((symbol (thing-at-point 'symbol t))
         (responses (canvas--sync-request "info" "symbol" (or symbol "")))
         (resp (car (last responses)))
         (name (cdr (assoc "name" resp)))
         (ns (cdr (assoc "ns" resp)))
         (file (cdr (assoc "file" resp)))
         (doc (cdr (assoc "doc" resp))))
    (if (not name)
        (message "No info for '%s'" symbol)
      (if (and current-prefix-arg file (file-exists-p file))
          ;; With prefix arg: jump to file
          (find-file file)
        ;; Without: show info in minibuffer
        (message "%s%s%s"
                 (if ns (format "[%s] " ns) "")
                 name
                 (if doc (format " -- %s" doc) ""))))))

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
    map))

;;;###autoload
(define-minor-mode canvas-mode
  "Minor mode for editing wasm-canvas .scm files with nREPL integration."
  :lighter " Canvas"
  :keymap canvas-mode-map
  (if canvas-mode
      (progn
        (add-hook 'completion-at-point-functions #'canvas-completions-at-point nil t)
        (unless canvas--session
          (when (canvas--port)
            (condition-case err
                (canvas-connect)
              (error (message "Canvas: %s" (error-message-string err)))))))
    (remove-hook 'completion-at-point-functions #'canvas-completions-at-point t)))

;; Auto-activate for .scm files under ~/.canvas/nodes/
(add-hook 'scheme-mode-hook
          (lambda ()
            (when (and buffer-file-name
                       (string-match-p "\\.canvas/nodes/" buffer-file-name))
              (canvas-mode 1))))

(provide 'canvas-mode)
;;; canvas-mode.el ends here
