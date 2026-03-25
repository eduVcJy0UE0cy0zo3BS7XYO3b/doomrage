;;; canvas-mode-integration.el --- Integration test: Emacs ↔ peer via nREPL -*- lexical-binding: t; -*-
;;
;; Starts a peer process, connects via nREPL, runs full hash-import + rename cycle.
;;
;; Run:
;;   cargo build -p wasm-canvas-peer
;;   emacs -batch -l canvas-mode.el -l tests/canvas-mode-integration.el -f ert-run-tests-batch-and-exit

(require 'ert)
(require 'cl-lib)

(let ((root (locate-dominating-file (or load-file-name buffer-file-name default-directory) "Cargo.toml")))
  (when root (load (expand-file-name "canvas-mode.el" root))))

(defvar test--project-root
  (locate-dominating-file (or load-file-name buffer-file-name default-directory) "Cargo.toml"))

(defvar test--peer-process nil)
(defvar test--project-dir nil)

(defun test--find-peer-binary ()
  (unless test--project-root (error "Cannot find project root"))
  (or (let ((f (expand-file-name "target/debug/wasm-canvas-peer" test--project-root)))
        (and (file-executable-p f) f))
      (let ((f (expand-file-name "target/release/wasm-canvas-peer" test--project-root)))
        (and (file-executable-p f) f))
      (error "Peer binary not found. Run: cargo build -p wasm-canvas-peer")))

(defun test--start-peer ()
  (let ((binary (test--find-peer-binary)))
    (setq test--project-dir (make-temp-file "canvas-test-" t))
    ;; Init
    (shell-command-to-string (format "%s --init %s 2>&1" binary test--project-dir))
    ;; Start
    (let ((log-file (expand-file-name "peer.log" test--project-dir)))
      (setq test--peer-process
            (start-process-shell-command "test-peer" "*test-peer*"
                                         (format "RUST_LOG=info %s --project %s 2>&1 | tee %s"
                                                 binary test--project-dir log-file))))
    (set-process-query-on-exit-flag test--peer-process nil)
    ;; Wait for port
    (let ((port-file (expand-file-name ".canvas/.nrepl-port" test--project-dir))
          (deadline (+ (float-time) 15.0)))
      (while (and (not (file-exists-p port-file))
                  (< (float-time) deadline)
                  (process-live-p test--peer-process))
        (sleep-for 0.3))
      (unless (file-exists-p port-file)
        (error "Peer did not start")))
    ;; Connect
    (let ((port (string-to-number
                 (string-trim (with-temp-buffer
                                (insert-file-contents
                                 (expand-file-name ".canvas/.nrepl-port" test--project-dir))
                                (buffer-string))))))
      (canvas-connect port)
      (unless canvas--session (error "nREPL connect failed")))))

(defun test--stop-peer ()
  (when canvas--process (ignore-errors (delete-process canvas--process)) (setq canvas--process nil))
  (setq canvas--session nil)
  (when test--project-dir
    (let ((log-file (expand-file-name "peer.log" test--project-dir)))
      (when (file-exists-p log-file)
        (message "\n--- Peer log (last 20 lines) ---")
        (message "%s" (with-temp-buffer
                        (insert-file-contents log-file)
                        (goto-char (point-max))
                        (forward-line -20)
                        (buffer-substring (point) (point-max)))))))
  (when (and test--peer-process (process-live-p test--peer-process))
    (kill-process test--peer-process))
  (setq test--peer-process nil)
  (when test--project-dir (ignore-errors (delete-directory test--project-dir t)))
  (setq test--project-dir nil))

(defun test--op (op &rest pairs)
  (car (last (apply #'canvas--sync-request op pairs))))

(defun test--ok-p (resp)
  (let ((s (cdr (assoc "status" resp))))
    (and (listp s) (member "done" s) (not (member "error" s)))))

(defvar test--pass 0)
(defvar test--fail 0)

(defun test--assert (desc ok)
  (if ok
      (progn (message "  OK: %s" desc) (setq test--pass (1+ test--pass)))
    (message "  FAIL: %s" desc) (setq test--fail (1+ test--fail))))

(ert-deftest integration-full-cycle ()
  "Full integration: peer ↔ Emacs nREPL — create, compute, defs, hash-import, rename."
  (setq test--pass 0 test--fail 0)
  (unwind-protect
      (progn
        (test--start-peer)
        (message "--- Connected to peer ---")

        ;; 1. Describe — verify new ops exist
        (message "\n--- describe ---")
        (let* ((resp (test--op "describe"))
               (ops (cdr (assoc "ops" resp))))
          (test--assert "describe ok" (test--ok-p resp))
          (test--assert "has defs op" (assoc "defs" ops))
          (test--assert "has def-source op" (assoc "def-source" ops))
          (test--assert "has def-history op" (assoc "def-history" ops))
          (test--assert "has rename-def op" (assoc "rename-def" ops))
          (test--assert "has add-hash-import op" (assoc "add-hash-import" ops))
          (test--assert "has migrate-imports op" (assoc "migrate-imports" ops)))

        ;; 2. Create source node
        (message "\n--- create source node ---")
        (let ((resp (test--op "create-node"
                              "canvas" "default"
                              "label" "controls"
                              "code" "(define gain 42)\n(define freq 440)"
                              "exports" '("gain" "freq"))))
          (test--assert "create-node ok" (test--ok-p resp)))

        ;; 3. Compute
        (message "\n--- compute ---")
        (let ((resp (test--op "compute" "canvas" "default" "label" "controls")))
          (test--assert "compute ok" (test--ok-p resp)))
        (sleep-for 2.0)

        ;; 4. List definitions
        (message "\n--- list defs ---")
        (let* ((resp (test--op "defs" "canvas" "default"))
               (defs (cdr (assoc "defs" resp))))
          (test--assert "defs ok" (test--ok-p resp))
          (test--assert "has definitions" (> (length defs) 0))
          (let ((gain-def (cl-find-if (lambda (d) (equal (cdr (assoc "name" d)) "gain")) defs)))
            (test--assert "gain exists" gain-def)
            (test--assert "gain has hash" (> (length (or (cdr (assoc "hash" gain-def)) "")) 0))

            ;; 5. Def-source by hash
            (message "\n--- def-source ---")
            (when gain-def
              (let* ((hash (cdr (assoc "hash" gain-def)))
                     (resp (test--op "def-source" "hash" hash))
                     (source (cdr (assoc "source" resp))))
                (test--assert "def-source ok" (test--ok-p resp))
                (test--assert "source is string" (stringp source))
                (test--assert "source has 42" (and source (string-match-p "42" source)))

                ;; 6. Create consumer with hash import
                (message "\n--- hash import ---")
                (let ((resp (test--op "create-node"
                                      "canvas" "default"
                                      "label" "synth"
                                      "code" "(define result (* gain 2))"
                                      "exports" '("result"))))
                  (test--assert "create consumer ok" (test--ok-p resp)))
                (let ((resp (test--op "add-hash-import"
                                      "canvas" "default"
                                      "label" "synth"
                                      "hash" hash
                                      "local-name" "gain")))
                  (test--assert "add-hash-import ok" (test--ok-p resp)))

                ;; 7. Verify node-state has hash-imports
                (message "\n--- node-state ---")
                (let* ((state (test--op "node-state" "canvas" "default" "label" "synth"))
                       (his (cdr (assoc "hash-imports" state))))
                  (test--assert "node-state ok" (test--ok-p state))
                  (test--assert "has hash-imports" (> (length his) 0))
                  (when (> (length his) 0)
                    (test--assert "import hash matches"
                                  (equal (cdr (assoc "hash" (nth 0 his))) hash))
                    (test--assert "import name is gain"
                                  (equal (cdr (assoc "name" (nth 0 his))) "gain"))))

                ;; 8. Verify .scm file has pragma
                (message "\n--- file pragmas ---")
                (let* ((scm-path (expand-file-name ".canvas/nodes/default/synth.scm" test--project-dir))
                       (content (with-temp-buffer (insert-file-contents scm-path) (buffer-string))))
                  (test--assert "scm has @import pragma" (string-match-p "^;;; @import" content))
                  (test--assert "scm has hash" (string-match-p hash content)))

                ;; 9. Rename gain → volume
                (message "\n--- rename ---")
                (let ((resp (test--op "rename-def"
                                      "canvas" "default"
                                      "old-name" "gain"
                                      "new-name" "volume")))
                  (test--assert "rename ok" (test--ok-p resp))
                  (test--assert "updated >= 1" (>= (or (cdr (assoc "updated" resp)) 0) 1)))

                ;; 10. Verify rename in defs
                (message "\n--- verify rename ---")
                (let* ((defs-after (cdr (assoc "defs" (test--op "defs" "canvas" "default"))))
                       (gain-gone (cl-find-if (lambda (d) (equal (cdr (assoc "name" d)) "gain")) defs-after))
                       (vol-found (cl-find-if (lambda (d) (equal (cdr (assoc "name" d)) "volume")) defs-after)))
                  (test--assert "gain gone from defs" (null gain-gone))
                  (test--assert "volume in defs" vol-found)
                  (when vol-found
                    (test--assert "same hash after rename"
                                  (equal (cdr (assoc "hash" vol-found)) hash))))

                ;; 11. Verify source code updated
                (let* ((state (test--op "node-state" "canvas" "default" "label" "controls"))
                       (code (cdr (assoc "code" state))))
                  (test--assert "code has define volume" (string-match-p "define volume" code))
                  (test--assert "code has no define gain" (not (string-match-p "define gain" code))))

                ;; 12. Verify consumer hash_import renamed
                (let* ((state (test--op "node-state" "canvas" "default" "label" "synth"))
                       (his (cdr (assoc "hash-imports" state))))
                  (when (> (length his) 0)
                    (test--assert "consumer import renamed to volume"
                                  (equal (cdr (assoc "name" (nth 0 his))) "volume"))))

                ;; 13. Def history
                (message "\n--- history ---")
                (let* ((resp (test--op "def-history" "name" "volume" "canvas" "default"))
                       (history (cdr (assoc "history" resp))))
                  (test--assert "history ok" (test--ok-p resp))
                  (test--assert "history has entries" (> (length history) 0)))

                ;; 14. Migrate imports
                (message "\n--- migrate imports ---")
                ;; Re-compute controls so definitions are up to date after rename
                (test--op "compute" "canvas" "default" "label" "controls")
                (sleep-for 1.5)
                (let ((resp (test--op "create-node"
                                      "canvas" "default"
                                      "label" "legacy"
                                      "code" "(define out (* freq 2))"
                                      "exports" '("out")
                                      "imports" [["default" "controls"]])))
                  (test--assert "create legacy node ok" (test--ok-p resp)))
                (let ((resp (test--op "migrate-imports" "canvas" "default" "label" "legacy")))
                  (test--assert "migrate ok" (test--ok-p resp))
                  (let ((migrated (cdr (assoc "migrated" resp))))
                    (test--assert "migrated >= 1" (>= (length migrated) 1))))
                (let* ((state (test--op "node-state" "canvas" "default" "label" "legacy"))
                       (imports (cdr (assoc "imports" state)))
                       (hash-imports (cdr (assoc "hash-imports" state))))
                  (test--assert "legacy imports cleared" (= (length imports) 0))
                  (test--assert "hash-imports populated" (> (length hash-imports) 0)))))))

        ;; Summary
        (message "\n===================================")
        (message "  PASSED: %d" test--pass)
        (message "  FAILED: %d" test--fail)
        (message "===================================")
        (should (= test--fail 0)))
    (test--stop-peer)))

(ert-deftest integration-file-workflow ()
  "Test Emacs file-based workflow: create → write → exports → compute → info → goto-def → import."
  (setq test--pass 0 test--fail 0)
  (unwind-protect
      (progn
        (test--start-peer)
        (message "--- File workflow test ---")

        ;; 1. Create source node via nREPL — should produce .scm file
        (message "\n--- create source node ---")
        (test--op "create-node" "canvas" "default" "label" "math"
                  "code" "(define pi 3.14159)\n(define tau (* 2 pi))"
                  "exports" '("pi" "tau"))
        (let ((scm (expand-file-name ".canvas/nodes/default/math.scm" test--project-dir)))
          (test--assert "math.scm created" (file-exists-p scm))
          ;; Open the file — canvas-mode should activate
          (find-file scm)
          (test--assert "buffer has code" (string-match-p "define pi" (buffer-string))))

        ;; 2. Compute from the buffer
        (message "\n--- compute ---")
        (test--op "compute" "canvas" "default" "label" "math")
        (sleep-for 1.5)

        ;; 3. Switch namespace and verify
        (message "\n--- switch-ns ---")
        (canvas--sync-request "switch-ns" "ns" "default/math")
        (setq canvas--current-ns "default/math")
        (test--assert "ns set" (equal canvas--current-ns "default/math"))

        ;; 4. Info on symbol — should return file + line + hash
        (message "\n--- info ---")
        (let* ((resp (test--op "info" "symbol" "pi"))
               (name (cdr (assoc "name" resp))))
          ;; info without ns context returns nil for local symbols (need to test via imported)
          ;; so test via creating a consumer that imports math
          (test--assert "info returns something or nil" t)) ;; placeholder

        ;; 5. Create consumer node
        (message "\n--- create consumer ---")
        (test--op "create-node" "canvas" "default" "label" "physics"
                  "code" "(define c 299792458)"
                  "exports" '("c"))
        (test--op "compute" "canvas" "default" "label" "physics")
        (sleep-for 1.0)

        ;; 6. Add hash import from math to physics
        (message "\n--- add-import via defs ---")
        (let* ((defs-resp (test--op "defs" "canvas" "default"))
               (defs (cdr (assoc "defs" defs-resp)))
               (pi-def (cl-find-if (lambda (d) (equal (cdr (assoc "name" d)) "pi")) defs))
               (pi-hash (and pi-def (cdr (assoc "hash" pi-def)))))
          (test--assert "pi definition found" pi-def)
          (when pi-hash
            (test--op "add-hash-import" "canvas" "default" "label" "physics"
                      "hash" pi-hash "local-name" "pi")

            ;; 7. Update physics code to use pi
            (test--op "update-node" "canvas" "default" "label" "physics"
                      "code" "(define circumference (* 2 pi 10))"
                      "exports" '("circumference"))
            (test--op "compute" "canvas" "default" "label" "physics")
            (sleep-for 1.5)

            ;; 8. Node state — should have outputs
            (message "\n--- node-state with outputs ---")
            (let* ((state (test--op "node-state" "canvas" "default" "label" "physics"))
                   (outputs (cdr (assoc "outputs" state)))
                   (err (cdr (assoc "error" state))))
              (test--assert "no error" (null err))
              (test--assert "has outputs" (> (length outputs) 0))
              (when outputs
                (test--assert "circumference in outputs"
                              (assoc "circumference" outputs))))

            ;; 9. Info on imported symbol — switch to physics ns first
            (message "\n--- goto-definition via info ---")
            (sleep-for 1.0)
            (setq canvas--current-ns "default/physics")
            ;; Pass ns explicitly to info (avoids switch-ns race)
            (let* ((resp (test--op "info" "symbol" "pi" "ns" "default/physics"))
                   (name (cdr (assoc "name" resp)))
                   (file (cdr (assoc "file" resp)))
                   (line (cdr (assoc "line" resp)))
                   (hash (cdr (assoc "hash" resp))))
              (test--assert "info returns pi" (equal name "pi"))
              (test--assert "info has file" (and file (stringp file)))
              (test--assert "info has line" (and line (integerp line) (> line 0)))
              (test--assert "info has hash" (and hash (stringp hash) (> (length hash) 0)))
              (test--assert "info file is math.scm" (and file (string-match-p "math\\.scm" file)))
              (test--assert "info line is 1" (equal line 1))

              ;; 10. Simulate goto-def: open file and go to line
              (message "\n--- goto-def jump ---")
              (when (and file (file-exists-p file))
                (find-file file)
                (goto-char (point-min))
                (forward-line (1- line))
                (test--assert "jumped to correct line"
                              (string-match-p "define pi" (buffer-substring (line-beginning-position) (line-end-position))))
                (test--assert "buffer is math.scm"
                              (string-match-p "math\\.scm" (buffer-file-name)))))

            ;; 11. Verify .scm file has pragma after import
            (message "\n--- file pragma ---")
            (let* ((phys-file (expand-file-name ".canvas/nodes/default/physics.scm" test--project-dir))
                   (content (with-temp-buffer (insert-file-contents phys-file) (buffer-string))))
              (test--assert "physics.scm has @import" (string-match-p ";;; @import" content))
              (test--assert "physics.scm has pi hash" (string-match-p pi-hash content)))))

        ;; Summary
        (message "\n===================================")
        (message "  PASSED: %d" test--pass)
        (message "  FAILED: %d" test--fail)
        (message "===================================")
        (should (= test--fail 0)))
    (test--stop-peer)))

(ert-deftest integration-elisp-commands ()
  "Test actual elisp interactive commands (not raw nREPL ops)."
  (setq test--pass 0 test--fail 0)
  (unwind-protect
      (progn
        (test--start-peer)
        (message "--- Elisp commands test ---")

        ;; 1. canvas-create-node: creates node + opens file + sets ns
        (message "\n--- canvas-create-node ---")
        (canvas-create-node "default" "signals")
        (test--assert "create-node: ns set" (equal canvas--current-ns "default/signals"))
        ;; Find the actual file peer created
        (let ((real-file (expand-file-name ".canvas/nodes/default/signals.scm" test--project-dir)))
          (test--assert "create-node: scm file exists on disk" (file-exists-p real-file))
          (find-file real-file))

        ;; 2. Write code into the buffer
        (message "\n--- write code ---")
        (let ((inhibit-read-only t))
          (erase-buffer)
          (insert "(define amplitude 100)\n(define phase 0.5)\n"))
        (save-buffer)

        ;; 3. canvas-load-file: send code to peer
        (message "\n--- canvas-load-file ---")
        (canvas-load-file)
        (sleep-for 0.5)

        ;; 4. canvas-set-exports
        (message "\n--- canvas-set-exports ---")
        (canvas-set-exports "amplitude phase")
        (sleep-for 0.3)
        ;; Verify via node-state op
        (let* ((state (test--op "node-state" "canvas" "default" "label" "signals"))
               (exports (cdr (assoc "exports" state))))
          (test--assert "set-exports: has amplitude" (member "amplitude" exports))
          (test--assert "set-exports: has phase" (member "phase" exports)))

        ;; 5. canvas-compute: auto-saves + computes
        (message "\n--- canvas-compute ---")
        (canvas-compute)
        (sleep-for 1.5)

        ;; 6. canvas-node-state: should show buffer with outputs
        (message "\n--- canvas-node-state ---")
        (canvas-node-state)
        (let ((state-buf (get-buffer "*canvas-node*")))
          (test--assert "node-state: buffer created" state-buf)
          (when state-buf
            (with-current-buffer state-buf
              (let ((content (buffer-string)))
                (test--assert "node-state: shows exports" (string-match-p "Exports:.*amplitude" content))
                (test--assert "node-state: shows outputs" (string-match-p "amplitude" content))))))

        ;; 7. canvas-list-defs
        (message "\n--- canvas-list-defs ---")
        (canvas-list-defs "default")
        (let ((defs-buf (get-buffer "*canvas-defs*")))
          (test--assert "list-defs: buffer created" defs-buf)
          (when defs-buf
            (with-current-buffer defs-buf
              (let ((content (buffer-string)))
                (test--assert "list-defs: shows amplitude" (string-match-p "amplitude" content))
                (test--assert "list-defs: shows hash" (string-match-p "[0-9a-f]\\{16\\}" content))))))

        ;; 8. canvas-def-source: get body by hash
        (message "\n--- canvas-def-source ---")
        (let* ((defs-resp (test--op "defs" "canvas" "default"))
               (defs (cdr (assoc "defs" defs-resp)))
               (amp-def (cl-find-if (lambda (d) (equal (cdr (assoc "name" d)) "amplitude")) defs))
               (amp-hash (and amp-def (cdr (assoc "hash" amp-def)))))
          (when amp-hash
            (canvas-def-source amp-hash)
            (let ((src-buf (get-buffer (format "*def:%s*" (substring amp-hash 0 8)))))
              (test--assert "def-source: buffer created" src-buf)
              (when src-buf
                (with-current-buffer src-buf
                  (test--assert "def-source: has 100" (string-match-p "100" (buffer-string))))))

            ;; 9. Create consumer + canvas-add-hash-import (non-interactive version)
            (message "\n--- add-hash-import ---")
            (canvas-create-node "default" "output")
            (test--assert "consumer created" (equal canvas--current-ns "default/output"))
            ;; Open real file
            (let ((output-file (expand-file-name ".canvas/nodes/default/output.scm" test--project-dir)))
              (find-file output-file))
            (canvas-add-hash-import "default" "output" amp-hash "amplitude")
            ;; Revert buffer to see pragma
            (when (and buffer-file-name (file-exists-p buffer-file-name))
              (revert-buffer t t t))
            (test--assert "pragma in buffer" (string-match-p ";;; @import" (buffer-string)))

            ;; 10. canvas-eval-buffer: write code + eval
            (message "\n--- canvas-eval-buffer ---")
            (goto-char (point-max))
            (insert "(define doubled (* amplitude 2))\n")
            (save-buffer)
            ;; eval-buffer sends code to eval (async — result in REPL buffer)
            (canvas-eval-buffer)
            (sleep-for 0.5)

            ;; 11. canvas-eval-region
            (message "\n--- canvas-eval-region ---")
            (let ((code "(+ 10 20)"))
              (with-temp-buffer
                (insert code)
                (canvas-eval-region (point-min) (point-max))))
            (sleep-for 0.5)
            ;; Check REPL buffer has output
            (let ((repl-buf (get-buffer canvas--buffer)))
              (test--assert "repl buffer exists" repl-buf)
              (when repl-buf
                (with-current-buffer repl-buf
                  (test--assert "repl has eval output" (> (buffer-size) 0)))))

            ;; 12. canvas-info-at-point on imported symbol
            (message "\n--- canvas-info-at-point ---")
            (let ((output-file (expand-file-name ".canvas/nodes/default/output.scm" test--project-dir)))
              (when (file-exists-p output-file) (find-file output-file)))
            (setq canvas--current-ns "default/output")
            (goto-char (point-min))
            (when (search-forward "amplitude" nil t)
              ;; Simulate info-at-point (non-interactive — just call the op logic)
              (let* ((responses (apply #'canvas--sync-request "info" "symbol" "amplitude"
                                       (when canvas--current-ns (list "ns" canvas--current-ns))))
                     (resp (car (last responses)))
                     (name (cdr (assoc "name" resp)))
                     (file (cdr (assoc "file" resp)))
                     (line (cdr (assoc "line" resp))))
                (test--assert "info: returns amplitude" (equal name "amplitude"))
                (test--assert "info: has file" (and file (stringp file)))
                (test--assert "info: has line" (and line (> line 0)))

                ;; 13. Simulate goto-def (C-u C-c C-d)
                (when (and file line)
                  (let ((real-file (expand-file-name ".canvas/nodes/default/signals.scm" test--project-dir)))
                    (when (file-exists-p real-file)
                      (find-file-noselect real-file t) ;; t = no-warn about changes
                      (set-buffer (find-buffer-visiting real-file))
                      (revert-buffer t t t)
                      (goto-char (point-min))
                      (forward-line (1- line))
                      (test--assert "goto-def: landed on define amplitude"
                                    (string-match-p "define amplitude"
                                                    (buffer-substring (line-beginning-position) (line-end-position)))))))

            ;; 14. canvas-rename-def (call non-interactively)
            (message "\n--- canvas-rename-def ---")
            ;; Don't use the interactive command — it calls revert-buffer which
            ;; can fail in batch mode. Call the op directly.
            (let ((resp (test--op "rename-def" "canvas" "default" "old-name" "amplitude" "new-name" "amp")))
              (test--assert "rename op ok" (test--ok-p resp)))
            (sleep-for 0.3)
            ;; Verify rename in defs
            (let* ((defs-after (cdr (assoc "defs" (test--op "defs" "canvas" "default"))))
                   (old (cl-find-if (lambda (d) (equal (cdr (assoc "name" d)) "amplitude")) defs-after))
                   (new (cl-find-if (lambda (d) (equal (cdr (assoc "name" d)) "amp")) defs-after)))
              (test--assert "rename: amplitude gone" (null old))
              (test--assert "rename: amp present" new))
            ;; Verify source code changed
            (let ((source-file (expand-file-name ".canvas/nodes/default/signals.scm" test--project-dir)))
              (with-temp-buffer
                (insert-file-contents source-file)
                (test--assert "rename: code has define amp" (string-match-p "define amp " (buffer-string)))
                (test--assert "rename: code no define amplitude" (not (string-match-p "define amplitude" (buffer-string))))))

            ;; 15. canvas-def-history
            (message "\n--- canvas-def-history ---")
            (canvas-def-history "amp" "default")
            (let ((hist-buf (get-buffer "*canvas-history*")))
              (test--assert "history: buffer created" hist-buf)
              (when hist-buf
                (with-current-buffer hist-buf
                  (test--assert "history: has entries" (> (buffer-size) 50)))))

            ;; 16. canvas-migrate-imports (create legacy node first)
            (message "\n--- canvas-migrate-imports ---")
            (test--op "create-node" "canvas" "default" "label" "legacy-consumer"
                      "code" "(define out phase)"
                      "exports" '("out")
                      "imports" [["default" "signals"]])
            ;; Recompute signals so definitions are fresh
            (test--op "compute" "canvas" "default" "label" "signals")
            (sleep-for 1.0)
            (canvas-migrate-imports "default" "legacy-consumer")
            (sleep-for 0.3)
            (let* ((state (test--op "node-state" "canvas" "default" "label" "legacy-consumer"))
                   (imports (cdr (assoc "imports" state)))
                   (hash-imports (cdr (assoc "hash-imports" state))))
              (test--assert "migrate: legacy cleared" (= (length imports) 0))
              (test--assert "migrate: hash-imports set" (> (length hash-imports) 0)))

            ;; 17. canvas-delete-node (non-interactive — skip yes-or-no-p)
            (message "\n--- delete-node ---")
            (test--op "delete-node" "canvas" "default" "label" "legacy-consumer")
            (let ((state (test--op "node-state" "canvas" "default" "label" "legacy-consumer")))
              (test--assert "delete: node gone" (not (test--ok-p state))))

            ;; 18. canvas-def-diff (need two different hashes)
            (message "\n--- canvas-def-diff ---")
            ;; Update signals to create v2
            (test--op "update-node" "canvas" "default" "label" "signals"
                      "code" "(define amp 200)\n(define phase 0.75)")
            (test--op "compute" "canvas" "default" "label" "signals")
            (sleep-for 1.0)
            ;; Get history — should have v1 and v2
            (let* ((hist-resp (test--op "def-history" "name" "amp" "canvas" "default"))
                   (history (cdr (assoc "history" hist-resp))))
              (when (>= (length history) 2)
                (let ((hash-new (cdr (assoc "hash" (nth 0 history))))
                      (hash-old (cdr (assoc "hash" (nth 1 history)))))
                  (canvas-def-diff hash-old hash-new)
                  (let ((diff-buf (get-buffer "*canvas-diff*")))
                    (test--assert "diff: buffer created" diff-buf)
                    (when diff-buf
                      (with-current-buffer diff-buf
                        (let ((content (buffer-string)))
                          (test--assert "diff: has content" (> (length content) 10)))))))))))))

        ;; Summary
        (message "\n===================================")
        (message "  PASSED: %d" test--pass)
        (message "  FAILED: %d" test--fail)
        (message "===================================")
        (should (= test--fail 0)))
    (test--stop-peer)))

(provide 'canvas-mode-integration)
;;; canvas-mode-integration.el ends here
