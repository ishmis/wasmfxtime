;;! stack_switching = true
;; test the handler type's behaviour with tables

(module
  (type $ht (handler))
  (type $f (func (param (ref null $ht))))
  (type $ct (cont $f))

  (func $print-i32 (import "spectest" "print_i32") (param i32))

  (tag $yield)

  (table $cont_tbl 0 (ref null $ct)) 
  (table $name_tbl 0 (ref null $ht))

  (func $f  (param $name (ref null $ht)) 
    (local $h (ref null $ht))
    (local $i i32)
    (table.set $name_tbl (i32.const 0) (local.get $name)) 
    (loop $next
      (suspend_to $ht $yield (table.get $name_tbl (i32.const 0)))
      (local.set $h)
      (table.set $name_tbl (i32.const 0) (local.get $h)) 
      (local.set $i (i32.add (i32.const 1) (local.get $i)))
      (br_if $next (i32.le_u (local.get $i) (i32.const 1)))
    )
    (call $print-i32 (i32.const 42))
    (return)
  )
  (elem declare func $f)

  ;; size checking
  (func (export "cont_tbl_size") (result i32)
    (table.size $cont_tbl)
  )

  (func (export "name_tbl_size") (result i32)
    (table.size $name_tbl)
  )

  ;; growth
  (func (export "cont_tbl_grow_f") (result i32)
    (table.grow $cont_tbl (cont.new $ct (ref.func $f)) (i32.const 10))
  )
  (func (export "name_tbl_grow_f") (result i32)
    (table.grow $name_tbl (ref.null $ht) (i32.const 10))
  )

  ;; check null
  (func (export "cont_tbl_null_at") (param $i i32) (result i32)
    (ref.is_null (table.get $cont_tbl (local.get $i)))
  )

  (func (export "name_tbl_null_at") (param $i i32) (result i32)
    (ref.is_null (table.get $name_tbl (local.get $i)))
  )

  ;; set 
  (func (export "cont_tbl_set_f") (param $i i32)
    (table.set $cont_tbl (local.get $i) (cont.new $ct (ref.func $f)))
  )

  (func (export "name_tbl_set_null") (param $i i32) 
    (table.set $name_tbl (local.get $i) (ref.null $ht))
  )

  ;; run 
  (func (export "cont_tbl_run_init") (param $i i32) 
    (local $h (ref null $ct))
    (block $on_yield (result (ref null $ct))
      (resume_with $ct (on $yield $on_yield) (i32.const 99) (table.get $cont_tbl (local.get $i)))
      (return)
    ) ;; on_yield
    (local.set $h)
    (table.set $cont_tbl (local.get $i) (local.get $h))
  )
)

;; grow the cont table to hold 10 conts
(assert_return (invoke "cont_tbl_size") (i32.const 0))
(assert_return (invoke "cont_tbl_grow_f") (i32.const 0))
(assert_return (invoke "cont_tbl_size") (i32.const 10))

;; grow the name table to hold 10 names
(assert_return (invoke "name_tbl_size") (i32.const 0))
(assert_return (invoke "name_tbl_grow_f") (i32.const 0))
(assert_return (invoke "name_tbl_size") (i32.const 10))
(assert_return (invoke "name_tbl_null_at" (i32.const 0)) (i32.const 1))



;; We now consume the continuation, do cont_tbl[0] := null and write a fresh
;; continuation to cont_tbl[9], which we then consume
(assert_return (invoke "cont_tbl_run_init" (i32.const 0)))
(assert_return (invoke "cont_tbl_run_init" (i32.const 0)))
(assert_return (invoke "cont_tbl_run_init" (i32.const 0)))
;; ensure cont is now completely consumed 
(assert_trap (invoke "cont_tbl_run_init" (i32.const 0)) "continuation already consumed") 


;; (assert_return (invoke "cont_tbl_null_at" (i32.const 9)) (i32.const 0))
;; (assert_return (invoke "cont_tbl_set_f" (i32.const 9)))
;; (assert_return (invoke "cont_tbl_run" (i32.const 9)) (i32.const 100))