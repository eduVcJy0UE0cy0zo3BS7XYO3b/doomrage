;;; wasm-canvas graph — remote oscilloscope
;;; Load this on a second computer in the same LAN.
;;; It receives gain/freq from the "controls" channel via libp2p.

(graph
  (viewport 0.0 0.0 1.0))

;;; --- nodes ---

(node 1 "Script" "remote wave" (pos 50.0 50.0)

  (define gain (net-value "controls" "gain" 50.0))
  (define freq (net-value "controls" "freq" 5.0))

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
      (draw-text 4 4 "remote oscilloscope" "#666666" 12)))

  @(render osc)

  Gain = @(->str gain), Freq = @(->str freq)
)
