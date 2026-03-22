;;; wasm-canvas graph

(graph
  (viewport 0.0 0.0 1.0))

;;; --- nodes ---

(node 1 "Script" "controls" (pos 50.0 50.0)

  (define gain (widget "gain" 'slider 0.0 100.0))
  (define freq (widget "freq" 'slider 1.0 20.0))

  # Controls

  Gain = @(->str gain), Freq = @(->str freq)
)

(node 2 "Script" "gateway out" (pos 300.0 50.0)

  (define gain (input 'gain 'f64))
  (define freq (input 'freq 'f64))
  (net-publish "controls")

  # Gateway Out

  Publishing gain=@(->str gain), freq=@(->str freq)
)

(node 3 "Script" "gateway in" (pos 550.0 50.0)

  (define gain (output 'gain 'f64))
  (define freq (output 'freq 'f64))
  (set! gain (net-value "controls" "gain" 50.0))
  (set! freq (net-value "controls" "freq" 5.0))

  # Gateway In

  Receiving gain=@(->str gain), freq=@(->str freq)
)

(node 4 "Script" "wave" (pos 800.0 50.0)

  (define gain-raw (input 'gain 'f64))
  (define freq-raw (input 'freq 'f64))
  (define gain (if (compute? gain-raw) 50.0 gain-raw))
  (define freq (if (compute? freq-raw) 5.0 freq-raw))

  (define pi 3.14159265)
  (define w 300.0)
  (define h 150.0)
  (define mid (/ h 2.0))
  (define n 60)

  (define (wave-points i acc)
    (if (= i n) acc
      (let* ((x (* (/ i n) w))
             (y (+ mid (* gain (sin (* freq (/ i n) pi 4.0))))))
        (wave-points (+ i 1) (cons (list x y) acc)))))

  (define pts (reverse (wave-points 0 '())))

  (define osc
    (canvas w h
      (draw-rect 0 0 w h "#f5f5f0")
      (draw-line 0 mid w mid "#cccccc" 1)
      (draw-polyline pts "#2266cc" 2)
      (draw-text 4 4 "oscilloscope" "#666666" 12)))

  @(render osc)

  Peak = @(->str gain), Freq = @(->str freq)
)

(node 5 "Script" "colors" (pos 50.0 350.0)

  (define swatch
    (canvas 200 80
      (draw-rect   0 0 40 40 "#e74c3c")
      (draw-rect  50 0 40 40 "#2ecc71")
      (draw-rect 100 0 40 40 "#3498db")
      (draw-rect 150 0 40 40 "#f39c12")
      (draw-text   5 50 "red"    "#e74c3c" 11)
      (draw-text  55 50 "green"  "#2ecc71" 11)
      (draw-text 100 50 "blue"   "#3498db" 11)
      (draw-text 150 50 "orange" "#f39c12" 11)))

  # Color Swatch

  @(render swatch)
)

(node 6 "Script" "app window" (pos 300.0 350.0)

  (open-window "My App")
  (define n (slider "value" 0 100))

  # My App

  Value = @(->str n)

  ---

  Move the slider in the separate window!
)

;;; --- connections ---

(connection 1 (from 1 "gain") (to 2 "gain"))
(connection 2 (from 1 "freq") (to 2 "freq"))
(connection 3 (from 3 "gain") (to 4 "gain"))
(connection 4 (from 3 "freq") (to 4 "freq"))
