;;! stack_switching = true
;; Summation example
(module
  (type $ht (handler))
  (type $ft (func (param (ref $ht))))
  (type $ct (cont $ft))

  (tag $yield (param i32))

  (func $nats (type $ft)
    (local $h (ref $ht))
    (local $i i32)
    (local.set $h (local.get 0))
    (loop $next
      (suspend_to $ht $yield
        (local.get $i) (local.get $h))
      (local.set $h)
      (local.set $i (i32.add (i32.const 1) (local.get $i)))
      (br $next)))

  (func (export "sumUp") (param $n i32) (result i32)
    (local $i i32)
    (local $j i32)
    (local $k (ref $ct))
    (local.set $k (cont.new $ct (ref.func $nats)))
    (loop $next
      (block $on_yield (result i32 (ref $ct))
        (resume_with $ct (on $yield $on_yield) (local.get $k))
        (return (local.get $i))
      ) ;; on_yield
      (local.set $k)
      (i32.add (local.get $i))
      (local.set $i)
      (local.set $j (i32.add (i32.const 1) (local.get $j)))
      (br_if $next (i32.le_u (local.get $j) (local.get $n)))
   )
   (return (local.get $i)))
   
   (elem declare func $nats)
)
(assert_return (invoke "sumUp" (i32.const 10)) (i32.const 55))
