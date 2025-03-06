use crate::emit_debug_assert;
use crate::emit_debug_assert_eq;
use crate::emit_debug_assert_icmp;
use crate::emit_debug_assert_ule;
use crate::emit_debug_println;
use crate::translate::FuncTranslationState;
use crate::wasmfx::optimized::typed_continuation_helpers as tc;

use crate::wasmfx::optimized::vmcontref_store_payloads;
use crate::wasmfx::optimized::vmctx_store_payloads;
use crate::wasmfx::optimized::ControlEffect;
use crate::wasmfx::shared;

use cranelift_codegen::ir;
use itertools::{Either, Itertools};

use cranelift_codegen::ir::condcodes::*;
use cranelift_codegen::ir::types::*;
use cranelift_codegen::ir::{Block, BlockCall, InstBuilder, JumpTableData};
use cranelift_frontend::FunctionBuilder;
use wasmtime_environ::PtrSize;
use wasmtime_environ::{WasmResult, WasmValType};

use super::optimized::typed_continuation_helpers::StackChain;

#[allow(clippy::cast_possible_truncation, reason = "TODO")]
fn vmcontref_load_return_values<'a>(
    env: &mut crate::func_environ::FuncEnvironment<'a>,
    builder: &mut FunctionBuilder,
    valtypes: &[WasmValType],
    contref: ir::Value,
) -> std::vec::Vec<ir::Value> {
    let co = tc::VMContRef::new(contref);
    let mut values = vec![];

    if valtypes.len() > 0 {
        let result_buffer_addr = co.get_results(env, builder);

        let mut offset = 0;
        let memflags = ir::MemFlags::trusted();
        for valtype in valtypes {
            let val = builder.ins().load(
                crate::value_type(env.isa, *valtype),
                memflags,
                result_buffer_addr,
                offset,
            );
            values.push(val);
            offset += env.offsets.ptr.maximum_value_size() as i32;
        }
    }
    return values;
}

/// Loads values of the given types from the `Payloads` object in the `VMContext`.
#[allow(clippy::cast_possible_truncation, reason = "TODO")]
fn vmctx_load_payloads<'a>(
    env: &mut crate::func_environ::FuncEnvironment<'a>,
    builder: &mut FunctionBuilder,
    valtypes: &[ir::Type],
) -> Vec<ir::Value> {
    let mut values = vec![];

    if valtypes.len() > 0 {
        let vmctx = env.vmctx_val(&mut builder.cursor());
        let vmctx_payloads = tc::Payloads::new(
            vmctx,
            env.offsets.vmctx_typed_continuations_payloads() as i32,
        );

        values = vmctx_payloads.load_data_entries(env, builder, valtypes);

        // In theory, we way want to deallocate the buffer instead of just
        // clearing it if its size is above a certain threshold. That would
        // avoid keeping a large object unnecessarily long.
        vmctx_payloads.clear(builder);
    }

    values
}

/// This function generates code that searches for a handler for `tag_address`,
/// which must be a `*mut VMTagDefinition`. The search walks up the chain of
/// continuations beginning at `start`.
///
/// The flag `search_suspend_handlers` determines whether we search for a
/// suspend or switch handler. Concretely, this influences which part of each
/// handler list we will search.
///
/// We trap if no handler was found.
///
/// The returned values are:
/// 1. The stack (continuation or main stack, represented as a StackChain) in
///    whose handler list we found the tag (i.e., the stack that performed the
///    resume instruction that installed handler for the tag).
/// 2. The continuation whose parent is the stack mentioned in 1.
/// 3. The index of the handler in the handler list.
///
/// In pseudo-code, the generated code's behavior can be expressed as
/// follows:
///
/// chain_link = start
/// while !chain_link.is_main_stack() {
///   contref = chain_link.get_contref()
///   parent_link = contref.parent
///   parent_csi = parent_link.get_common_stack_information();
///   handlers = parent_csi.handlers;
///   (begin_range, end_range) = if search_suspend_handlers {
///     (0, parent_csi.first_switch_handler_index)
///   } else {
///     (parent_csi.first_switch_handler_index, handlers.length)
///   }; --> ignore this bit coz no switch
///
///   add name checking here first and only then brif to tag checking below:  
///
///   for index in begin_range..end_range {
///     if handlers[index] == tag_address {
///       goto on_match(contref, index)
///     }
///   }
///   chain_link = parent_link
/// }
/// trap(unhandled_tag)
///
/// on_match(conref : VMContRef, handler_index : u32)
/// ... execution continues here here ...
///
fn search_named_handler<'a>(
    env: &mut crate::func_environ::FuncEnvironment<'a>,
    builder: &mut FunctionBuilder,
    start: &tc::StackChain,
    handler_addr: ir::Value,
    tag_address: ir::Value,
) -> (StackChain, ir::Value, ir::Value) {
    let handle_link = builder.create_block();
    let begin_search_handler_list = builder.create_block();
    let iter_tags = builder.create_block();
    let try_index = builder.create_block();
    let compare_tags = builder.create_block();
    let on_match = builder.create_block();
    let on_no_match = builder.create_block();

    // Terminate previous block:
    builder.ins().jump(handle_link, &start.to_raw_parts());

    // Block handle_link
    let chain_link = {
        builder.append_block_param(handle_link, env.pointer_type());
        builder.append_block_param(handle_link, env.pointer_type());
        builder.switch_to_block(handle_link);

        let raw_parts = builder.block_params(handle_link);
        let chain_link =
            tc::StackChain::from_raw_parts([raw_parts[0], raw_parts[1]], env.pointer_type());
        let is_main_stack = chain_link.is_main_stack(env, builder);
        builder.ins().brif(
            is_main_stack,
            on_no_match,
            &[],
            begin_search_handler_list,
            &[],
        );
        chain_link
    };

    // Block begin_search_handler_list
    {
        builder.switch_to_block(begin_search_handler_list);
        let contref = chain_link.unchecked_get_continuation(env, builder);
        let vmcontref = tc::VMContRef::new(contref);

        let parent_link = vmcontref.get_parent_stack_chain(env, builder);

        // check names match
        let names_match = builder.ins().icmp(IntCC::Equal, handler_addr, contref);
        emit_debug_println!(
            env,
            builder,
            "[search_named_handler] handler_addr: {:p}, contref_addr: {:p}, names_match = {}",
            handler_addr,
            contref,
            names_match
        );
        builder.ins().brif(
            names_match,
            iter_tags,
            &[contref],
            handle_link,
            &parent_link.to_raw_parts(),
        );
    }

    let (contref, parent_link, handler_list_data_ptr, end_range) = {
        builder.append_block_param(iter_tags, I64);
        builder.switch_to_block(iter_tags);
        let contref = builder.block_params(iter_tags)[0];
        let contref = tc::VMContRef::new(contref);

        let parent_link = contref.get_parent_stack_chain(env, builder);

        emit_debug_println!(
            env,
            builder,
            "[search_handler] beginning search in parent of continuation {:p}",
            contref.address
        );

        let parent_csi = parent_link.get_common_stack_information(env, builder);

        let handlers = parent_csi.get_handler_list();
        let handler_list_data_ptr = handlers.get_data(env, builder);

        let first_switch_handler_index = parent_csi.get_first_switch_handler_index(env, builder);

        // Note that these indices are inclusive-exclusive, i.e. [begin_range, end_range).
        let (begin_range, end_range) = {
            let zero = builder.ins().iconst(I32, 0);
            if cfg!(debug_assertions) {
                let length = handlers.get_length(env, builder);
                emit_debug_assert_ule!(env, builder, first_switch_handler_index, length);
            }
            (zero, first_switch_handler_index)
        };
        builder.ins().jump(try_index, &[begin_range]);

        (contref, parent_link, handler_list_data_ptr, end_range)
    };

    // Block try_index
    let index = {
        builder.append_block_param(try_index, I32);
        builder.switch_to_block(try_index);
        let index = builder.block_params(try_index)[0];

        let in_bounds = builder
            .ins()
            .icmp(IntCC::UnsignedLessThan, index, end_range);
        builder.ins().brif(
            in_bounds,
            compare_tags,
            &[],
            handle_link,
            &parent_link.to_raw_parts(),
        );
        index
    };

    // Block compare_tags
    {
        builder.switch_to_block(compare_tags);

        let base = handler_list_data_ptr;
        let entry_size = std::mem::size_of::<*mut u8>();
        let offset = builder.ins().imul_imm(index, entry_size as i64);
        let offset = builder.ins().uextend(I64, offset);
        let entry_address = builder.ins().iadd(base, offset);

        let memflags = ir::MemFlags::trusted();

        let handled_tag = builder
            .ins()
            .load(env.pointer_type(), memflags, entry_address, 0);

        let tags_match = builder.ins().icmp(IntCC::Equal, handled_tag, tag_address);
        let incremented_index = builder.ins().iadd_imm(index, 1);
        builder
            .ins()
            .brif(tags_match, on_match, &[], try_index, &[incremented_index]);
    }

    // Block on_no_match
    {
        builder.switch_to_block(on_no_match);
        builder.set_cold_block(on_no_match);
        builder.ins().trap(crate::TRAP_UNHANDLED_NAMED_OR_TAG);
    }

    builder.seal_block(handle_link);
    builder.seal_block(begin_search_handler_list);
    builder.seal_block(iter_tags);
    builder.seal_block(try_index);
    builder.seal_block(compare_tags);
    builder.seal_block(on_match);
    builder.seal_block(on_no_match);

    // final block: on_match
    builder.switch_to_block(on_match);

    emit_debug_println!(
        env,
        builder,
        "[search_handler] found handler at stack chain ({}, {:p}), whose child continuation is {:p}, index is {}",
        parent_link.to_raw_parts()[0],
        parent_link.to_raw_parts()[1],
        contref.address,
        index
    );

    (parent_link, contref.address, index)
}

pub(crate) fn translate_resume_with<'a>(
    env: &mut crate::func_environ::FuncEnvironment<'a>,
    state: &mut FuncTranslationState,
    builder: &mut FunctionBuilder<'_>,
    arity: usize,
    type_index: u32,
    resumetable: &[(u32, Option<ir::Block>)],
) -> WasmResult<Vec<ir::Value>> {
    // The resume instruction is the most involved instruction to
    // compile as it is responsible for both continuation application
    // and control tag dispatch.
    //
    // Here we translate a resume instruction into several basic
    // blocks as follows:
    //
    //        previous block
    //              |
    //              |
    //        resume_block
    //         /           \
    //        /             \
    //        |             |
    //  return_block        |
    //                suspend block
    //                      |
    //                dispatch block
    //
    // * resume_block handles continuation arguments and performs
    //   actual stack switch. On ordinary return from resume, it jumps
    //   to the `return_block`, whereas on suspension it jumps to the
    //   `suspend_block`.
    // * suspend_block is used on suspension, jumps onward to
    //   `dispatch_block`.
    // * dispatch_block uses a jump table to dispatch to actual
    //   user-defined handler blocks, based on the handler index
    //   provided on suspension. Note that we do not jump to the
    //   handler blocks directly. Instead, each handler block has a
    //   corresponding premable block, which we jump to in order to
    //   reach a particular handler block. The preamble block prepares
    //   the arguments and continuation object to be passed to the
    //   actual handler block.
    //
    let resume_block = builder.create_block();
    let return_block = builder.create_block();
    let suspend_block = builder.create_block();
    let dispatch_block = builder.create_block();

    let vmctx = tc::VMContext::new(env.vmctx_val(&mut builder.cursor()), env.pointer_type());

    // Split the resumetable into suspend handlers (each represented by the tag
    // index and handler block) and the switch handlers (represented just by the
    // tag index). Note that we currently don't remove duplicate tags.
    let (suspend_handlers, switch_tags): (Vec<(u32, Block)>, Vec<u32>) = resumetable
        .iter()
        .partition_map(|(tag_index, block_opt)| match block_opt {
            Some(block) => Either::Left((*tag_index, *block)),
            None => Either::Right(*tag_index),
        });

    // Technically, there is no need to have a dedicated resume block, we could
    // just put all of its contents into the current block.
    builder.ins().jump(resume_block, &[]);

    // Resume block: actually resume the continuation chain ending at `resume_contref`.
    let (resume_result, vm_runtime_limits_ptr, original_stack_chain, new_stack_chain) = {
        builder.switch_to_block(resume_block);
        builder.seal_block(resume_block);

        let resume_contobj = state.pop1();

        let (witness, resume_contref) = shared::disassemble_contobj(env, builder, resume_contobj);

        let mut vmcontref = tc::VMContRef::new(resume_contref);

        let revision = vmcontref.get_revision(env, builder);
        let evidence = builder.ins().icmp(IntCC::Equal, revision, witness);
        emit_debug_println!(
            env,
            builder,
            "[resume_with] resume_contref = {:p} witness = {}, revision = {}, evidence = {}",
            resume_contref,
            witness,
            revision,
            evidence
        );
        builder
            .ins()
            .trapz(evidence, crate::TRAP_CONTINUATION_ALREADY_CONSUMED);
        let next_revision = vmcontref.incr_revision(env, builder, revision);
        emit_debug_println!(
            env,
            builder,
            "[resume_with] new revision = {}",
            next_revision
        );

        if cfg!(debug_assertions) {
            // This should be impossible due to the linearity check.
            let zero = builder.ins().iconst(I8, 0);
            let csi = vmcontref.common_stack_information(env, builder);
            let has_returned = csi.has_state(env, builder, wasmtime_continuations::State::Returned);
            emit_debug_assert_eq!(env, builder, has_returned, zero);
        }

        state.push2(resume_contref, resume_contobj);
        let (_, resume_args) = state.peekn(arity + 1).split_last().unwrap();

        let count = builder.ins().iconst(I32, resume_args.len() as i64);
        
        // current solution is to just use the vmcontref as the handler name directly
        vmcontref_store_payloads(env, builder, &resume_args, count, resume_contref);

        // Splice together stack chains:
        // Connect the end of the chain starting at `resume_contref` to the currently active chain.
        let mut last_ancestor = tc::VMContRef::new(vmcontref.get_last_ancestor(env, builder));

        // Make the currently running continuation (if any) the parent of the one we are about to resume.
        let original_stack_chain =
            tc::VMContext::new(vmctx.address, env.pointer_type()).load_stack_chain(env, builder);
        original_stack_chain.assert_not_absent(env, builder);
        if cfg!(debug_assertions) {
            // The continuation we are about to resume should have its chain broken up at last_ancestor.
            let last_ancestor_chain = last_ancestor.get_parent_stack_chain(env, builder);
            let is_absent = last_ancestor_chain.is_absent(env, builder);
            emit_debug_assert!(env, builder, is_absent);
        }
        last_ancestor.set_parent_stack_chain(env, builder, &original_stack_chain);

        emit_debug_println!(
            env,
            builder,
            "[resume_with] spliced together stack chains: parent of {:p} (last ancestor of {:p}) is now pointing to ({}, {:p})",
            last_ancestor.address,
            vmcontref.address,
            original_stack_chain.to_raw_parts()[0],
            original_stack_chain.to_raw_parts()[1]
        );

        // Just for consistency: `vmcontref` is about to get state Running, so let's zero out its last_ancestor field.
        let zero = builder.ins().iconst(env.pointer_type(), 0);
        vmcontref.set_last_ancestor(env, builder, zero);

        // We mark `resume_contref` as the currently running one
        vmctx.set_active_continuation(env, builder, resume_contref);

        // Note that the resume_contref libcall a few lines further below
        // manipulates the stack limits as follows:
        // 1. Copy stack_limit, last_wasm_entry_sp and last_wasm_exit* values from
        // VMRuntimeLimits into the currently active continuation (i.e., the
        // one that will become the parent of the to-be-resumed one)
        //
        // 2. Copy `stack_limit` and `last_wasm_entry_sp` in the
        // `StackLimits` of `resume_contref` into the `VMRuntimeLimits`.
        //
        // See the comment on `wasmtime_continuations::StackChain` for a
        // description of the invariants that we maintain for the various stack
        // limits.

        // `resume_contref` is now active, and its parent is suspended.
        let resume_contref = tc::VMContRef::new(resume_contref);
        let resume_csi = resume_contref.common_stack_information(env, builder);
        let parent_csi = original_stack_chain.get_common_stack_information(env, builder);
        resume_csi.set_state(env, builder, wasmtime_continuations::State::Running);
        parent_csi.set_state(env, builder, wasmtime_continuations::State::Parent);

        // We update the `StackLimits` of the parent of the continuation to be resumed
        // as well as the `VMRuntimeLimits`.
        // See the comment on `wasmtime_continuations::StackChain` for a description
        // of the invariants that we maintain for the various stack limits.
        let vm_runtime_limits_ptr = vmctx.load_vm_runtime_limits_ptr(env, builder);
        parent_csi.load_limits_from_vmcontext(env, builder, vm_runtime_limits_ptr, true);
        resume_csi.write_limits_to_vmcontext(env, builder, vm_runtime_limits_ptr);

        // Install handlers in (soon to be) parent's HandlerList:
        // Let the i-th handler clause be (on $tag $block).
        // Then the i-th entry of the HandlerList will be the address of $tag.
        let handler_list = parent_csi.get_handler_list();

        if resumetable.len() > 0 {
            // Total number of handlers (suspend and switch).
            let handler_count = builder.ins().iconst(I32, resumetable.len() as i64);

            // If the existing list is too small, reallocate (in runtime).
            handler_list.ensure_capacity(env, builder, handler_count);

            let suspend_handler_count = suspend_handlers.len();

            // All handlers, represented by the indices of the tags they handle.
            // All the suspend handlers come first, followed by all the switch handlers.
            let all_handlers = suspend_handlers
                .iter()
                .map(|(tag_index, _block)| *tag_index)
                .chain(switch_tags);

            // Translate all tag indices to tag addresses (i.e., the corresponding *mut VMTagDefinition).
            let all_tag_addresses: Vec<ir::Value> = all_handlers
                .map(|tag_index| shared::tag_address(env, builder, tag_index))
                .collect();

            // Store all tag addresess in the handler list.
            handler_list.store_data_entries(env, builder, &all_tag_addresses, false);

            // To enable distinguishing switch and suspend handlers when searching the handler list:
            // Store at which index the switch handlers start.
            let first_switch_handler_index =
                builder.ins().iconst(I32, suspend_handler_count as i64);
            parent_csi.set_first_switch_handler_index(env, builder, first_switch_handler_index);
        }

        let resume_payload = ControlEffect::make_resume(env, builder).to_u64();

        // Note that the control context we use for switching is not the one in
        // (the stack of) resume_contref, but in (the stack of) last_ancestor!
        let fiber_stack = last_ancestor.get_fiber_stack(env, builder);
        let control_context_ptr = fiber_stack.load_control_context(env, builder);

        emit_debug_println!(
            env,
            builder,
            "[resume_with] about to execute stack_switch, control_context_ptr is {:p}",
            control_context_ptr
        );

        let result =
            builder
                .ins()
                .stack_switch(control_context_ptr, control_context_ptr, resume_payload);

        emit_debug_println!(
            env,
            builder,
            "[resume_with] continuing after stack_switch in frame with parent_stack_chain ({}, {:p}), result is {:p}",
            original_stack_chain.to_raw_parts()[0],
            original_stack_chain.to_raw_parts()[1],
            result
        );

        // At this point we know nothing about the continuation that just
        // suspended or returned. In particular, it does not have to be what we
        // called `resume_contref` earlier on. We must reload the information
        // about the now active continuation from the VMContext.
        let new_stack_chain = vmctx.load_stack_chain(env, builder);

        // Now the parent contref (or main stack) is active again
        vmctx.store_stack_chain(env, builder, &original_stack_chain);
        parent_csi.set_state(env, builder, wasmtime_continuations::State::Running);

        // Just for consistency: Reset the handler list.
        handler_list.clear(builder);
        parent_csi.set_first_switch_handler_index(env, builder, zero);

        // Extract the result and signal bit.
        let result = ControlEffect::from_u64(result);
        let signal = result.signal(env, builder);

        emit_debug_println!(
            env,
            builder,
            "[resume_with] in resume block, signal is {}",
            signal
        );

        // Jump to the return block if the result signal is 0, otherwise jump to
        // the suspend block.
        builder
            .ins()
            .brif(signal, suspend_block, &[], return_block, &[]);

        (
            result,
            vm_runtime_limits_ptr,
            original_stack_chain,
            new_stack_chain,
        )
    };

    // The suspend block: Only used when we suspended, not for returns.
    // Here we extract the index of the handler to use.
    let (handler_index, suspended_contobj) = {
        builder.switch_to_block(suspend_block);
        builder.seal_block(suspend_block);

        let suspended_continuation = new_stack_chain.unchecked_get_continuation(env, builder);
        let mut suspended_continuation = tc::VMContRef::new(suspended_continuation);
        let suspended_csi = suspended_continuation.common_stack_information(env, builder);

        // Note that at the suspend site, we already
        // 1. Set the state of suspended_continuation to Suspended
        // 2. Set suspended_continuation.last_ancestor
        // 3. Broke the continuation chain at suspended_continuation.last_ancestor

        // We store parts of the VMRuntimeLimits into the continuation that just suspended.
        suspended_csi.load_limits_from_vmcontext(env, builder, vm_runtime_limits_ptr, false);

        // Afterwards (!), restore parts of the VMRuntimeLimits from the
        // parent of the suspended continuation (which is now active).
        let parent_csi = original_stack_chain.get_common_stack_information(env, builder);
        parent_csi.write_limits_to_vmcontext(env, builder, vm_runtime_limits_ptr);

        // Extract the handler index
        let handler_index = ControlEffect::handler_index(resume_result, env, builder);

        let revision = suspended_continuation.get_revision(env, builder);
        let suspended_contobj =
            shared::assemble_contobj(env, builder, revision, suspended_continuation.address);

        emit_debug_println!(
            env,
            builder,
            "[resume_with] in suspend block, handler index is {}, new continuation is {:p}, with existing revision {}",
            handler_index,
            suspended_continuation.address,
            revision
        );

        // We need to terminate this block before being allowed to switch to
        // another one.
        builder.ins().jump(dispatch_block, &[]);

        (handler_index, suspended_contobj)
    };

    // For technical reasons, the jump table needs to have a default
    // block. In our case, it should be unreachable, since the handler
    // index we dispatch on should correspond to a an actual handler
    // block in the jump table.
    let jt_default_block = builder.create_block();
    {
        builder.switch_to_block(jt_default_block);
        builder.set_cold_block(jt_default_block);

        builder.ins().trap(crate::TRAP_UNREACHABLE);
    }

    // We create a preamble block for each of the actual handler blocks: It
    // reads the necessary arguments and passes them to the actual handler
    // block, together with the continuation object.
    let target_preamble_blocks = {
        let mut preamble_blocks = vec![];

        for &(handle_tag, target_block) in &suspend_handlers {
            let preamble_block = builder.create_block();
            preamble_blocks.push(preamble_block);
            builder.switch_to_block(preamble_block);

            let param_types = env.tag_params(handle_tag);
            let param_types: Vec<ir::Type> = param_types
                .iter()
                .map(|wty| crate::value_type(env.isa, *wty))
                .collect();
            let mut args = vmctx_load_payloads(env, builder, &param_types);
            args.push(suspended_contobj);

            builder.ins().jump(target_block, &args);
        }

        preamble_blocks
    };

    // Dispatch block. All it does is jump to the right premable block based on
    // the handler index.
    {
        builder.switch_to_block(dispatch_block);
        builder.seal_block(dispatch_block);

        let default_bc = builder.func.dfg.block_call(jt_default_block, &[]);

        let adapter_bcs: Vec<BlockCall> = target_preamble_blocks
            .iter()
            .map(|b| builder.func.dfg.block_call(*b, &[]))
            .collect();

        let jt_data = JumpTableData::new(default_bc, &adapter_bcs);
        let jt = builder.create_jump_table(jt_data);

        builder.ins().br_table(handler_index, jt);

        for preamble_block in target_preamble_blocks {
            builder.seal_block(preamble_block);
        }
        builder.seal_block(jt_default_block);
    }

    // Return block: Jumped to by resume block if continuation
    // returned normally.
    {
        builder.switch_to_block(return_block);
        builder.seal_block(return_block);

        // If we got a return signal, a continuation must have been running.
        let returned_contref = new_stack_chain.unchecked_get_continuation(env, builder);
        let returned_contref = tc::VMContRef::new(returned_contref);

        // Restore parts of the VMRuntimeLimits from the parent of the
        // returned continuation (which is now active).
        let parent_csi = original_stack_chain.get_common_stack_information(env, builder);
        parent_csi.write_limits_to_vmcontext(env, builder, vm_runtime_limits_ptr);

        let returned_csi = returned_contref.common_stack_information(env, builder);
        returned_csi.set_state(env, builder, wasmtime_continuations::State::Returned);

        // Load and push the results.
        let returns = env.continuation_returns(type_index).to_vec();
        let values = vmcontref_load_return_values(env, builder, &returns, returned_contref.address);

        // The continuation has returned and all `VMContObjs` to it
        // should have been be invalidated. We may safely deallocate
        // it. NOTE(dhil): it is only safe to deallocate the stack
        // object if there are no lingering references to it,
        // otherwise we have to keep it alive (though it can be
        // repurposed).
        shared::typed_continuations_drop_cont_ref(env, builder, returned_contref.address);

        Ok(values)
    }
}

/// Loads values of the given types from the continuation's `values` field.
#[allow(clippy::cast_possible_truncation, reason = "TODO")]
fn vmcontref_load_values_named<'a>(
    env: &mut crate::func_environ::FuncEnvironment<'a>,
    builder: &mut FunctionBuilder,
    contref: ir::Value,
    valtypes: &[WasmValType],
) -> Vec<ir::Value> {
    let memflags = ir::MemFlags::trusted();
    let mut result = vec![];

    if valtypes.len() > 0 {
        let co = tc::VMContRef::new(contref);
        let values = co.values();

        let payload_ptr = values.get_data(env, builder);

        let mut offset = 0;
        for valtype in valtypes {
            let val = builder.ins().load(
                crate::value_type(env.isa, *valtype),
                memflags,
                payload_ptr,
                offset,
            );
            result.push(val);
            offset += env.offsets.ptr.maximum_value_size() as i32;
        }

        emit_debug_println!(
            env,
            builder,
            "[vmcontref_load_values]: going to be clearing values!",
        );
    }

    // ishmis: always clear?
    tc::VMContRef::new(contref).values().clear(builder);

    result
}

pub(crate) fn translate_suspend_to<'a>(
    env: &mut crate::func_environ::FuncEnvironment<'a>,
    builder: &mut FunctionBuilder,
    tag_index: u32,
    hdlobj: ir::Value,
    suspend_args: &[ir::Value],
    tag_return_types: &[WasmValType],
) -> Vec<ir::Value> {
    let vmhandlerref = tc::VMContRef::new(hdlobj);
    emit_debug_println!(
        env,
        builder,
        "[suspend_to] suspending with name {:p}",
        hdlobj
    );

    vmctx_store_payloads(env, builder, suspend_args);

    let tag_addr = shared::tag_address(env, builder, tag_index);
    emit_debug_println!(
        env,
        builder,
        "[suspend_to] suspending with tag {:p}",
        tag_addr
    );

    let vmctx = env.vmctx_val(&mut builder.cursor());
    let vmctx = tc::VMContext::new(vmctx, env.pointer_type());
    let active_stack_chain = vmctx.load_stack_chain(env, builder);

    let (_, end_of_chain_contref, handler_index) = search_named_handler(
        env,
        builder,
        &active_stack_chain,
        vmhandlerref.address,
        tag_addr,
    );

    emit_debug_println!(
        env,
        builder,
        "[suspend_to] found handler: end of chain contref is {:p}, handler index is {}",
        end_of_chain_contref,
        handler_index
    );

    // If we get here, the search_handler logic succeeded (i.e., did not trap).
    // Thus, there is at least one parent, so we are not on the main stack.
    // Can therefore extract continuation directly.
    let active_contref = active_stack_chain.unchecked_get_continuation(env, builder);
    let active_contref = tc::VMContRef::new(active_contref);
    let mut end_of_chain_contref = tc::VMContRef::new(end_of_chain_contref);

    active_contref.set_last_ancestor(env, builder, end_of_chain_contref.address);

    // Set current continuation to suspended and break up handler chain.
    let active_contref_csi = active_contref.common_stack_information(env, builder);
    if cfg!(debug_assertions) {
        let is_running =
            active_contref_csi.has_state(env, builder, wasmtime_continuations::State::Running);
        emit_debug_assert!(env, builder, is_running);
    }

    active_contref_csi.set_state(env, builder, wasmtime_continuations::State::Suspended);
    let absent_chain_link = StackChain::absent(builder, env.pointer_type());
    end_of_chain_contref.set_parent_stack_chain(env, builder, &absent_chain_link);

    let suspend_payload = ControlEffect::make_suspend(env, builder, handler_index).to_u64();

    // Note that the control context we use for switching is the one
    // at the end of the chain, not the one in active_contref!
    // This also means that stack_switch saves the information about
    // the current stack in the control context located in the stack
    // of end_of_chain_contref.
    let fiber_stack = end_of_chain_contref.get_fiber_stack(env, builder);
    let control_context_ptr = fiber_stack.load_control_context(env, builder);

    builder
        .ins()
        .stack_switch(control_context_ptr, control_context_ptr, suspend_payload);

    let mut return_values =
        vmcontref_load_values_named(env, builder, active_contref.address, tag_return_types);

    // ishmis: return name too
    return_values.push(hdlobj);

    return_values
}
