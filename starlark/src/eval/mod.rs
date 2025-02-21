/*
 * Copyright 2018 The Starlark in Rust Authors.
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Evaluate some code, typically done by creating an [`Evaluator`], then calling
//! [`eval_module`](Evaluator::eval_module).

use std::{intrinsics::unlikely, mem, time::Instant};

pub(crate) use compiler::scope::ScopeNames;
pub(crate) use fragment::def::{Def, FrozenDef};
use gazebo::prelude::*;
pub use runtime::{
    arguments::{Arguments, ParametersParser, ParametersSpec},
    evaluator::Evaluator,
    file_loader::{FileLoader, ReturnFileLoader},
};

use crate::{
    collections::symbol_map::Symbol,
    environment::Globals,
    eval::{
        compiler::{
            scope::{CompilerAstMap, Scope, ScopeData},
            Compiler, Constants,
        },
        fragment::def::DefInfo,
    },
    syntax::ast::AstModule,
    values::{docs::DocString, Value},
};

pub(crate) mod bc;
mod compiler;
mod fragment;
pub(crate) mod runtime;
pub use runtime::profile::ProfileMode;

#[cfg(test)]
mod tests;

impl<'v, 'a> Evaluator<'v, 'a> {
    /// Evaluate an [`AstModule`] with this [`Evaluator`], modifying the in-scope
    /// [`Module`](crate::environment::Module) as appropriate.
    pub fn eval_module(&mut self, ast: AstModule, globals: &Globals) -> anyhow::Result<Value<'v>> {
        let start = Instant::now();

        let AstModule { codemap, statement } = ast;

        let codemap = self
            .module_env
            .frozen_heap()
            .alloc_any_display_from_debug(codemap.dupe());

        let globals = self.module_env.frozen_heap().alloc_any(globals.dupe());

        let mut scope_data = ScopeData::new();

        let root_scope_id = scope_data.new_scope().0;

        let mut statement = statement.into_map_payload(&mut CompilerAstMap(&mut scope_data));

        if let Some(docstring) = DocString::extract_raw_starlark_docstring(&statement) {
            self.module_env.set_docstring(docstring)
        }

        let mut scope = Scope::enter_module(
            self.module_env.names(),
            root_scope_id,
            scope_data,
            &mut statement,
            globals,
            codemap,
        );

        // We want to grab the first error only, with ownership, so drop all but the first
        scope.errors.truncate(1);
        if let Some(e) = scope.errors.pop() {
            // Static errors, reported even if the branch is not hit
            return Err(e);
        }

        let (module_slots, scope_names, scope_data) = scope.exit_module();
        let local_count = scope_names.used.len().try_into().unwrap();

        self.module_env.slots().ensure_slots(module_slots);
        let old_def_info = mem::replace(
            &mut self.def_info,
            self.module_env.frozen_heap().alloc_any(DefInfo::for_module(
                codemap,
                scope_names,
                globals,
            )),
        );

        // Set up the world to allow evaluation (do NOT use ? from now on)

        self.call_stack.push(Value::new_none(), None).unwrap();
        if unlikely(self.heap_or_flame_profile) {
            self.heap_profile
                .record_call_enter(Value::new_none(), self.heap());
            self.flame_profile.record_call_enter(Value::new_none());
        }

        // Evaluation
        let mut compiler = Compiler {
            scope_data,
            locals: Vec::new(),
            globals,
            codemap,
            constants: Constants::new(),
            has_before_stmt: self.before_stmt.enabled(),
            bc_profile: self.bc_profile.enabled(),
            eval: self,
        };

        let res = compiler.eval_module(statement, local_count);

        // Clean up the world, putting everything back
        self.call_stack.pop();
        if unlikely(self.heap_or_flame_profile) {
            self.heap_profile.record_call_exit(self.heap());
            self.flame_profile.record_call_exit();
        }
        self.def_info = old_def_info;

        self.module_env.add_eval_duration(start.elapsed());

        // Return the result of evaluation
        res.map_err(|e| e.0)
    }

    /// Evaluate a function stored in a [`Value`], passing in `positional` and `named` arguments.
    pub fn eval_function(
        &mut self,
        function: Value<'v>,
        positional: &[Value<'v>],
        named: &[(&str, Value<'v>)],
    ) -> anyhow::Result<Value<'v>> {
        let names = named.map(|(s, _)| (Symbol::new(*s), self.heap().alloc_str(*s)));
        let named = named.map(|x| x.1);
        let params = Arguments {
            pos: positional,
            named: &named,
            names: &names,
            args: None,
            kwargs: None,
        };
        function.invoke(&params, self)
    }
}
