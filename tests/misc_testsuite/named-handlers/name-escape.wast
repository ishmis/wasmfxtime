;;! stack_switching = true
;; some name escaping is prevented
(module
  (type $ht (handler))
  (type $f (func (param (ref $ht)) (result (ref $ht))))
  (type $k (cont $f))

  (tag $e)

  (func $f (type $f)
    (return (local.get 0)) ;; return name right away
  )

  (func (export "name_escape")
    (local $escaped_name (ref $ht))
    (block $h (result (ref $k))
      (resume_with $k (on $e $h) (cont.new $k (ref.func $f)))
      (local.set $escaped_name)
      (suspend_to $ht $e (local.get $escaped_name)) ;; trying to suspend to an escaped name errors!
      (return)
    )
    (drop)
  )

  (elem declare func $f)
)
(assert_trap (invoke "name_escape") "unhandled name or tag")

(module
  (type $ht (handler))
  (type $f (func (param (ref $ht)) (result (ref $ht))))
  (type $k (cont $f))

  (tag $e)

  (func $f (type $f)
    (suspend_to $ht $e (local.get 0)) 
    (return) 
  )

  (func (export "name_escape_with_suspend")
    (local $kf (ref $k))
    (local $escaped_name (ref $ht))
    (local.set $kf (cont.new $k (ref.func $f)))
    (loop $loop 
        (block $h (result (ref $k))
            (resume_with $k (on $e $h) (local.get $kf))
            (local.set $escaped_name)
            (suspend_to $ht $e (local.get $escaped_name)) 
            (return)
        ) ;; on e
        (local.set $kf)
        (br $loop)
    )
  ) 
  (elem declare func $f)
)
(assert_trap (invoke "name_escape_with_suspend") "unhandled name or tag")