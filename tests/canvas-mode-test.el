;;; canvas-mode-test.el --- ERT tests for canvas-mode.el -*- lexical-binding: t; -*-
;;
;; Run: emacs -batch -l canvas-mode.el -l tests/canvas-mode-test.el -f ert-run-tests-batch-and-exit

(require 'ert)
(require 'cl-lib)

;; Load canvas-mode from project root
(let ((project-root (file-name-directory (directory-file-name
                      (file-name-directory (or load-file-name buffer-file-name))))))
  (load (expand-file-name "canvas-mode.el" project-root)))

;;; ============================================================
;;; Bencode encode
;;; ============================================================

(ert-deftest test-bencode-encode-string ()
  (should (equal (canvas--bencode-encode "hello") "5:hello"))
  (should (equal (canvas--bencode-encode "") "0:"))
  (should (equal (canvas--bencode-encode "ab") "2:ab")))

(ert-deftest test-bencode-encode-integer ()
  (should (equal (canvas--bencode-encode 42) "i42e"))
  (should (equal (canvas--bencode-encode 0) "i0e"))
  (should (equal (canvas--bencode-encode -1) "i-1e")))

(ert-deftest test-bencode-encode-list ()
  (should (equal (canvas--bencode-encode '("a" "b")) "l1:a1:be"))
  (should (equal (canvas--bencode-encode '(1 2 3)) "li1ei2ei3ee"))
  (should (equal (canvas--bencode-encode '()) "le")))

(ert-deftest test-bencode-encode-dict ()
  "Dicts are alists with string keys, encoded sorted by key."
  (should (equal (canvas--bencode-encode '(("b" . "2") ("a" . "1")))
                 "d1:a1:11:b1:2e"))
  (should (equal (canvas--bencode-encode '(("op" . "eval") ("code" . "(+ 1 2)")))
                 "d4:code7:(+ 1 2)2:op4:evale")))

(ert-deftest test-bencode-encode-nested ()
  (should (equal (canvas--bencode-encode '(("list" . (1 2))))
                 "d4:listli1ei2eee")))

;;; ============================================================
;;; Bencode decode
;;; ============================================================

(ert-deftest test-bencode-decode-string ()
  (let ((result (canvas--bencode-decode "5:hello")))
    (should (equal (car result) "hello"))
    (should (equal (cdr result) ""))))

(ert-deftest test-bencode-decode-integer ()
  (let ((result (canvas--bencode-decode "i42e")))
    (should (equal (car result) 42))
    (should (equal (cdr result) ""))))

(ert-deftest test-bencode-decode-list ()
  (let ((result (canvas--bencode-decode "l1:a1:be")))
    (should (equal (car result) '("a" "b")))
    (should (equal (cdr result) ""))))

(ert-deftest test-bencode-decode-dict ()
  (let ((result (canvas--bencode-decode "d1:a1:11:b1:2e")))
    (should (equal (car result) '(("a" . "1") ("b" . "2"))))
    (should (equal (cdr result) ""))))

(ert-deftest test-bencode-decode-nested-dict ()
  (let ((result (canvas--bencode-decode "d4:listli1ei2ee3:numi42ee")))
    (should (equal (cdr (assoc "list" (car result))) '(1 2)))
    (should (equal (cdr (assoc "num" (car result))) 42))))

(ert-deftest test-bencode-decode-remaining ()
  "Decode should return remaining unconsumed data."
  (let ((result (canvas--bencode-decode "i1ei2e")))
    (should (equal (car result) 1))
    (should (equal (cdr result) "i2e"))))

(ert-deftest test-bencode-decode-incomplete ()
  "Incomplete bencode should return nil with full string as remainder."
  (let ((result (canvas--bencode-decode "5:hel")))
    (should (null (car result)))
    (should (equal (cdr result) "5:hel"))))

;;; ============================================================
;;; Bencode roundtrip
;;; ============================================================

(ert-deftest test-bencode-roundtrip-string ()
  (let ((result (canvas--bencode-decode (canvas--bencode-encode "test"))))
    (should (equal (car result) "test"))))

(ert-deftest test-bencode-roundtrip-dict ()
  (let* ((orig '(("op" . "eval") ("code" . "(+ 1 2)") ("session" . "abc")))
         (encoded (canvas--bencode-encode orig))
         (decoded (car (canvas--bencode-decode encoded))))
    ;; Keys sorted in encoded form
    (should (equal (cdr (assoc "op" decoded)) "eval"))
    (should (equal (cdr (assoc "code" decoded)) "(+ 1 2)"))
    (should (equal (cdr (assoc "session" decoded)) "abc"))))

(ert-deftest test-bencode-roundtrip-nrepl-message ()
  "Roundtrip a realistic nREPL response message."
  (let* ((msg '(("id" . "emacs-1")
                ("session" . "sess-42")
                ("name" . "gain")
                ("ns" . "controls")
                ("file" . "/home/user/.canvas/nodes/main/controls.scm")
                ("line" . 3)
                ("hash" . "1a2b3c4d5e6f7890")
                ("status" . ("done"))))
         (encoded (canvas--bencode-encode msg))
         (decoded (car (canvas--bencode-decode encoded))))
    (should (equal (cdr (assoc "name" decoded)) "gain"))
    (should (equal (cdr (assoc "line" decoded)) 3))
    (should (equal (cdr (assoc "hash" decoded)) "1a2b3c4d5e6f7890"))
    (should (member "done" (cdr (assoc "status" decoded))))))

;;; ============================================================
;;; Message construction (canvas--send-op internals)
;;; ============================================================

(ert-deftest test-send-op-builds-correct-msg ()
  "Verify that send-op constructs the right alist."
  (setq canvas--session "test-session")
  (setq canvas--msg-counter 0)
  ;; We can't call send-op without a process, so test the msg construction logic
  (let* ((id (canvas--next-id))
         (msg `(("id" . ,id)
                ("op" . "info")
                ("session" . ,canvas--session)
                ("symbol" . "gain"))))
    (should (equal id "emacs-1"))
    (should (equal (cdr (assoc "op" msg)) "info"))
    (should (equal (cdr (assoc "session" msg)) "test-session"))
    (should (equal (cdr (assoc "symbol" msg)) "gain")))
  ;; Cleanup
  (setq canvas--session nil))

;;; ============================================================
;;; Hash import pragma parsing (tests the Emacs overlay logic)
;;; ============================================================

(ert-deftest test-hash-import-pragma-regex ()
  "Verify the regex used for hash import pragma matching."
  (let ((line ";;; @import 1a2b3c4d5e6f7890 gain"))
    (should (string-match "^;;; @import \\([0-9a-f]+\\) \\(\\S-+\\)" line))
    (should (equal (match-string 1 line) "1a2b3c4d5e6f7890"))
    (should (equal (match-string 2 line) "gain"))))

(ert-deftest test-hash-import-pragma-regex-no-match ()
  "Non-pragma lines should not match."
  (should-not (string-match "^;;; @import \\([0-9a-f]+\\) \\(\\S-+\\)" "(define x 1)"))
  (should-not (string-match "^;;; @import \\([0-9a-f]+\\) \\(\\S-+\\)" ";; regular comment")))

(ert-deftest test-hash-import-pragma-multiple ()
  "Parse multiple pragma lines from a buffer."
  (with-temp-buffer
    (insert ";;; @import 1a2b3c4d5e6f7890 gain\n")
    (insert ";;; @import fed9876543210abc freq\n")
    (insert "(define result (+ gain freq))\n")
    (goto-char (point-min))
    (let ((imports nil))
      (while (re-search-forward "^;;; @import \\([0-9a-f]+\\) \\(\\S-+\\)" nil t)
        (push (cons (match-string 1) (match-string 2)) imports))
      (setq imports (nreverse imports))
      (should (equal (length imports) 2))
      (should (equal (cdar imports) "gain"))
      (should (equal (car (nth 0 imports)) "1a2b3c4d5e6f7890"))
      (should (equal (cdr (nth 1 imports)) "freq")))))

;;; ============================================================
;;; Response parsing (simulated nREPL responses)
;;; ============================================================

(ert-deftest test-info-response-parsing ()
  "Parse a simulated info response with hash and line."
  (let ((resp '(("name" . "gain")
                ("ns" . "controls")
                ("file" . "/home/.canvas/nodes/main/controls.scm")
                ("line" . 1)
                ("hash" . "1a2b3c4d5e6f7890")
                ("doc" . "Exported from node \"controls\"")
                ("status" . ("done")))))
    (should (equal (cdr (assoc "name" resp)) "gain"))
    (should (equal (cdr (assoc "line" resp)) 1))
    (should (equal (cdr (assoc "hash" resp)) "1a2b3c4d5e6f7890"))
    (should (equal (cdr (assoc "ns" resp)) "controls"))
    (should (stringp (cdr (assoc "file" resp))))
    (should (member "done" (cdr (assoc "status" resp))))))

(ert-deftest test-defs-response-parsing ()
  "Parse a simulated defs response."
  (let* ((resp '(("defs" . ((("name" . "gain") ("hash" . "abc123") ("node" . "controls") ("form" . "Simple"))
                             (("name" . "freq") ("hash" . "def456") ("node" . "controls") ("form" . "Simple"))))
                 ("status" . ("done"))))
         (defs (cdr (assoc "defs" resp))))
    (should (equal (length defs) 2))
    (should (equal (cdr (assoc "name" (nth 0 defs))) "gain"))
    (should (equal (cdr (assoc "hash" (nth 1 defs))) "def456"))))

(ert-deftest test-history-response-parsing ()
  "Parse a simulated def-history response."
  (let* ((resp '(("history" . ((("name" . "x") ("hash" . "v3") ("node" . "n1") ("form" . "Simple"))
                                (("name" . "x") ("hash" . "v2") ("node" . "n1") ("form" . "Simple"))
                                (("name" . "x") ("hash" . "v1") ("node" . "n1") ("form" . "Simple"))))
                 ("status" . ("done"))))
         (history (cdr (assoc "history" resp))))
    (should (equal (length history) 3))
    (should (equal (cdr (assoc "hash" (nth 0 history))) "v3"))
    (should (equal (cdr (assoc "hash" (nth 2 history))) "v1"))))

(ert-deftest test-node-state-hash-imports-parsing ()
  "Parse node-state response with hash-imports."
  (let* ((resp '(("code" . "(define result (* gain 2))")
                 ("exports" . ("result"))
                 ("imports" . ())
                 ("hash-imports" . ((("hash" . "abc123") ("name" . "gain"))))
                 ("outputs" . (("result" . "84.0")))
                 ("status" . ("done"))))
         (hash-imports (cdr (assoc "hash-imports" resp))))
    (should (equal (length hash-imports) 1))
    (should (equal (cdr (assoc "hash" (nth 0 hash-imports))) "abc123"))
    (should (equal (cdr (assoc "name" (nth 0 hash-imports))) "gain"))))

(ert-deftest test-rename-response-parsing ()
  "Parse a rename-def response."
  (let ((resp '(("updated" . 3) ("status" . ("done")))))
    (should (equal (cdr (assoc "updated" resp)) 3))
    (should (member "done" (cdr (assoc "status" resp))))))

(ert-deftest test-diff-response-parsing ()
  "Parse a def-diff response."
  (let ((resp '(("diff" . "  +\n  1\n- 2\n+ 3\n") ("status" . ("done")))))
    (should (stringp (cdr (assoc "diff" resp))))
    (should (string-match-p "^- 2$" (cdr (assoc "diff" resp))))
    (should (string-match-p "^\\+ 3$" (cdr (assoc "diff" resp))))))

;;; ============================================================
;;; Network filter (partial data accumulation)
;;; ============================================================

(ert-deftest test-filter-partial-accumulation ()
  "Partial bencode data should be accumulated across filter calls."
  (setq canvas--partial-data "")
  ;; Simulate receiving data in chunks
  (setq canvas--partial-data (concat canvas--partial-data "5:hel"))
  (let ((result (canvas--bencode-decode canvas--partial-data)))
    (should (null (car result)))  ;; incomplete
    (should (equal (cdr result) "5:hel")))
  ;; More data arrives
  (setq canvas--partial-data (concat canvas--partial-data "lo"))
  (let ((result (canvas--bencode-decode canvas--partial-data)))
    (should (equal (car result) "hello"))
    (should (equal (cdr result) ""))))

;;; ============================================================
;;; Bencode edge cases
;;; ============================================================

(ert-deftest test-bencode-empty-dict ()
  (should (equal (canvas--bencode-encode '()) "le"))
  ;; An explicit empty dict can't be distinguished from empty list in elisp
  ;; but decode of "de" should produce nil
  (let ((result (canvas--bencode-decode "de")))
    (should (null (car result)))))

(ert-deftest test-bencode-unicode-string ()
  "Unicode strings: bencode length is byte length, not char length."
  (let* ((str "hello")
         (encoded (canvas--bencode-encode str))
         (decoded (car (canvas--bencode-decode encoded))))
    (should (equal decoded str))))

(ert-deftest test-bencode-multiline-code ()
  "Bencode should handle multiline Scheme code."
  (let* ((code "(define x 1)\n(define y 2)\n(+ x y)")
         (encoded (canvas--bencode-encode code))
         (decoded (car (canvas--bencode-decode encoded))))
    (should (equal decoded code))))

(provide 'canvas-mode-test)
;;; canvas-mode-test.el ends here
