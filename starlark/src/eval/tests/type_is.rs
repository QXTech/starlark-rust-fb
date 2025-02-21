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

//! Test for type-is optimizations.

use crate as starlark;
use crate::{
    assert::Assert,
    environment::GlobalsBuilder,
    eval::{Def, FrozenDef},
    values::{Value, ValueLike},
};

#[starlark_module]
fn globals(builder: &mut GlobalsBuilder) {
    fn returns_type_is(value: Value<'v>) -> anyhow::Result<bool> {
        Ok(if let Some(def) = value.downcast_ref::<FrozenDef>() {
            def.def_info.inline_def_body.is_some()
        } else if let Some(def) = value.downcast_ref::<Def>() {
            def.def_info.inline_def_body.is_some()
        } else {
            panic!("not def")
        })
    }
}

#[test]
fn returns_type_is() {
    let mut a = Assert::new();
    a.globals_add(globals);

    a.module(
        "types.star",
        "\
def is_list(x):
  return type(x) == type([])
",
    );

    a.pass(
        "\
load('types.star', 'is_list')
assert_true(returns_type_is(is_list))
assert_true(is_list([]))
assert_false(is_list({}))
    ",
    );
}

#[test]
fn does_not_return_type_is() {
    let mut a = Assert::new();
    a.globals_add(globals);
    a.pass(
        "\
def is_not_list(x):
  return type(x) != type([])

def something_else(x, y):
  return type(x) == type([])

assert_false(returns_type_is(is_not_list))
assert_false(returns_type_is(something_else))
    ",
    );
}
