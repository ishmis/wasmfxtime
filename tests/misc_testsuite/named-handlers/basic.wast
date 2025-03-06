;;! stack_switching = true
;; Basic named handlers tests

(module
  (tag $e)

  (type $ht (handler))
  (type $f (func (param (ref $ht))))
  (type $g (func (param)))
  (type $k (cont $f))
  (type $k2 (cont $g))

  (func $f (type $f)
    (suspend_to $ht $e (local.get 0))
    (drop) ;; drop named handler
  )

  (func (export "handled_proper")
    (block $h (result (ref $k))
      (resume_with $k (on $e $h) (cont.new $k (ref.func $f)))
      (unreachable)
    )
    (drop)
  ) 
  
  (elem declare func $f)
)
(assert_return (invoke "handled_proper"))


(module
  (type $ht (handler (result i32 i64 f32 f64)))
  (type $ft (func (param (ref $ht))))
  (type $ct (cont $ft))

  (func $noop (type $ft))
  (elem declare func $noop)

  (func $make-cont (result (ref $ct))
     (cont.new $ct (ref.func $noop)))

  (func $f (export "f") (result i32)
     (call $make-cont)
     (ref.is_null))
)
(assert_return (invoke "f") (i32.const 0))