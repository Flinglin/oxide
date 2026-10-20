;;; oxide-free-type.el --- oxide terminal editor integration -*- lexical-binding: t; -*-

;; This adapter reports explicit Emacs/Evil editing state and installs bounded
;; operation keys used only after oxide receives a current heartbeat.

(defvar oxide-free-type--last-state nil)
(defvar oxide-free-type--heartbeat nil)

(defun oxide-free-type--private-osc-available-p ()
  "Return non-nil only when private OSC is isolated to an oxide session."
  (let ((term-program (downcase (or (getenv "TERM_PROGRAM") "")))
        (lc-terminal (downcase (or (getenv "LC_TERMINAL") ""))))
    (and (= (length (or (getenv "TMUX") "")) 0)
         (= (length (or (getenv "STY") "")) 0)
         (= (length (or (getenv "ZELLIJ") "")) 0)
         (or (equal (getenv "oxide_PRIVATE_OSC") "1")
             (equal term-program "oxide")
             (equal lc-terminal "oxide")))))

(defun oxide-free-type--write-terminal (payload)
  "Write PAYLOAD only across an isolated oxide terminal boundary."
  (when (oxide-free-type--private-osc-available-p)
    (send-string-to-terminal payload)))

(defun oxide-free-type--mode-name ()
  (if (bound-and-true-p evil-local-mode)
      (pcase evil-state
        ('normal "normal")
        ('insert "insert")
        ('replace "replace")
        ('visual "visual")
        (_ "emacs"))
    "emacs"))

(defun oxide-free-type--selection-name ()
  (if (use-region-p) "region" "none"))

(defun oxide-free-type--state-payload (active)
  (format "\e]7719;v=3;kind=editor-state;app=emacs;mode=%s;selection=%s;caps=mouse,clipboard,edit;active=%d\a"
          (oxide-free-type--mode-name)
          (oxide-free-type--selection-name)
          (if active 1 0)))

(defun oxide-free-type--emit-state (&optional force)
  (let ((payload (oxide-free-type--state-payload t)))
    (when (or force (not (equal payload oxide-free-type--last-state)))
      (setq oxide-free-type--last-state payload)
      (oxide-free-type--write-terminal payload))))

(defun oxide-free-type--percent-encode (text)
  ;; Encode every UTF-8 byte so the OSC field grammar never depends on URI
  ;; libraries choosing which printable characters to preserve.
  (mapconcat (lambda (byte) (format "%%%02X" byte))
             (string-to-list (encode-coding-string text 'utf-8))
             ""))

(defun oxide-free-type--emit-clipboard (operation text)
  (oxide-free-type--write-terminal
   (format "\e]7719;v=3;kind=editor-clipboard;app=emacs;op=%s;data=%s\a"
           operation
           (oxide-free-type--percent-encode text))))

(defun oxide-free-type-copy-selection ()
  (interactive)
  (when (use-region-p)
    (let ((text (buffer-substring-no-properties
                 (region-beginning) (region-end))))
      (kill-new text)
      (oxide-free-type--emit-clipboard "copy" text)))
  (oxide-free-type--emit-state t))

(defun oxide-free-type-cut-selection ()
  (interactive)
  (when (use-region-p)
    (let ((text (buffer-substring-no-properties
                 (region-beginning) (region-end))))
      (kill-region (region-beginning) (region-end))
      (oxide-free-type--emit-clipboard "cut" text)))
  (oxide-free-type--emit-state t))

(defun oxide-free-type-prepare-paste ()
  (interactive)
  (when (use-region-p)
    (delete-region (region-beginning) (region-end)))
  (oxide-free-type--emit-state t))

(defun oxide-free-type-delete-selection ()
  (interactive)
  (when (use-region-p)
    (delete-region (region-beginning) (region-end)))
  (oxide-free-type--emit-state t))

(defvar oxide-free-type-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map [oxide-free-type-copy]
                #'oxide-free-type-copy-selection)
    (define-key map [oxide-free-type-cut]
                #'oxide-free-type-cut-selection)
    (define-key map [oxide-free-type-paste]
                #'oxide-free-type-prepare-paste)
    (define-key map [oxide-free-type-delete]
                #'oxide-free-type-delete-selection)
    map))

;;;###autoload
(define-minor-mode oxide-free-type-mode
  "Expose explicit terminal editing state to oxide Free Type Mode."
  :global t
  :keymap oxide-free-type-mode-map
  (if oxide-free-type-mode
      (progn
        (define-key input-decode-map "\e[99;5~" [oxide-free-type-copy])
        (define-key input-decode-map "\e[99;6~" [oxide-free-type-cut])
        (define-key input-decode-map "\e[99;7~" [oxide-free-type-paste])
        (define-key input-decode-map "\e[99;8~" [oxide-free-type-delete])
        (when (fboundp 'xterm-mouse-mode)
          (xterm-mouse-mode 1))
        (add-hook 'post-command-hook #'oxide-free-type--emit-state)
        (setq oxide-free-type--heartbeat
              (run-at-time 0 1 #'oxide-free-type--emit-state t)))
    (remove-hook 'post-command-hook #'oxide-free-type--emit-state)
    (when (timerp oxide-free-type--heartbeat)
      (cancel-timer oxide-free-type--heartbeat))
    (setq oxide-free-type--heartbeat nil)
    (oxide-free-type--write-terminal (oxide-free-type--state-payload nil))))

(add-hook 'kill-emacs-hook
          (lambda ()
            (when oxide-free-type-mode
              (oxide-free-type--write-terminal
               (oxide-free-type--state-payload nil)))))

(provide 'oxide-free-type)
;;; oxide-free-type.el ends here
