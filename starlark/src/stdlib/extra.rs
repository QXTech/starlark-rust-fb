/*
 * Copyright 2019 The Starlark in Rust Authors.
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

use std::{
    collections::HashSet,
    fmt::{self, Display},
};

use gazebo::{
    any::AnyLifetime,
    coerce::{coerce_ref, Coerce},
    prelude::*,
};
use itertools::Itertools;

use crate::{
    self as starlark,
    collections::symbol_map::Symbol,
    environment::GlobalsBuilder,
    eval::{Arguments, Evaluator},
    values::{
        dict::DictRef, function::FUNCTION_TYPE, none::NoneType, tuple::Tuple, Freeze, Freezer,
        FrozenStringValue, FrozenValue, StarlarkValue, StringValue, StringValueLike, Trace, Value,
        ValueLike,
    },
};

#[starlark_module]
pub fn filter(builder: &mut GlobalsBuilder) {
    fn filter(ref func: Value, ref seq: Value) -> anyhow::Result<Value<'v>> {
        let mut res = Vec::new();

        for v in seq.iterate(heap)? {
            if func.is_none() {
                if !v.is_none() {
                    res.push(v);
                }
            } else if func.invoke_pos(&[v], eval)?.to_bool() {
                res.push(v);
            }
        }
        Ok(heap.alloc_list(&res))
    }
}

#[starlark_module]
pub fn map(builder: &mut GlobalsBuilder) {
    fn map(ref func: Value, ref seq: Value) -> anyhow::Result<Value<'v>> {
        let it = seq.iterate(heap)?;
        let mut res = Vec::with_capacity(it.size_hint().0);
        for v in it {
            res.push(func.invoke_pos(&[v], eval)?);
        }
        Ok(heap.alloc_list(&res))
    }
}

#[starlark_module]
pub fn partial(builder: &mut GlobalsBuilder) {
    fn partial(
        ref func: Value,
        args: Value<'v>,
        kwargs: DictRef<'v>,
    ) -> anyhow::Result<Partial<'v>> {
        debug_assert!(Tuple::from_value(args).is_some());
        let names = kwargs
            .keys()
            .map(|x| {
                let x = StringValue::new(x).unwrap();
                (
                    // We duplicate string here.
                    // If this becomes hot, we should do better.
                    Symbol::new_hashed(x.unpack_starlark_str().as_str_hashed()),
                    x,
                )
            })
            .collect();
        Ok(Partial {
            func,
            pos: args,
            named: kwargs.values().collect(),
            names,
        })
    }
}

#[starlark_module]
pub fn debug(builder: &mut GlobalsBuilder) {
    /// Print the value with full debug formatting. The result may not be stable over time,
    /// mostly intended for debugging purposes.
    fn debug(ref val: Value) -> anyhow::Result<String> {
        Ok(format!("{:?}", val))
    }
}

#[starlark_module]
pub fn dedupe(builder: &mut GlobalsBuilder) {
    /// Remove duplicates in a list. Uses identity of value (pointer),
    /// rather than by equality.
    fn dedupe(ref val: Value) -> anyhow::Result<Value<'v>> {
        let mut seen = HashSet::new();
        let mut res = Vec::new();
        for v in val.iterate(heap)? {
            let p = v.ptr_value();
            if !seen.contains(&p) {
                seen.insert(p);
                res.push(v);
            }
        }
        Ok(heap.alloc_list(&res))
    }
}

struct PrintWrapper<'a, 'b>(&'a Vec<Value<'b>>);
impl fmt::Display for PrintWrapper<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, v) in self.0.iter().enumerate() {
            if i != 0 {
                f.write_str(" ")?;
            }
            fmt::Display::fmt(v, f)?;
        }
        Ok(())
    }
}

/// Invoked from `print` or `pprint` to print a value.
pub trait PrintHandler {
    /// If this function returns error, evaluation fails with this error.
    fn println(&self, text: &str) -> anyhow::Result<()>;
}

pub(crate) struct StderrPrintHandler;

impl PrintHandler for StderrPrintHandler {
    fn println(&self, text: &str) -> anyhow::Result<()> {
        eprintln!("{}", text);
        Ok(())
    }
}

#[starlark_module]
pub fn print(builder: &mut GlobalsBuilder) {
    fn print(args: Vec<Value>) -> anyhow::Result<NoneType> {
        // In practice most users should want to put the print somewhere else, but this does for now
        // Unfortunately, we can't use PrintWrapper because strings to_str() and Display are different.
        eval.print_handler
            .println(&args.iter().map(|x| x.to_str()).join(" "))?;
        Ok(NoneType)
    }
}

#[starlark_module]
pub fn pprint(builder: &mut GlobalsBuilder) {
    fn pprint(args: Vec<Value>) -> anyhow::Result<NoneType> {
        // In practice most users may want to put the print somewhere else, but this does for now
        eval.print_handler
            .println(&format!("{:#}", PrintWrapper(&args)))?;
        Ok(NoneType)
    }
}

#[starlark_module]
pub fn json(builder: &mut GlobalsBuilder) {
    fn json(ref x: Value) -> anyhow::Result<String> {
        x.to_json()
    }
}

#[starlark_module]
pub fn abs(builder: &mut GlobalsBuilder) {
    fn abs(ref x: i32) -> anyhow::Result<i32> {
        Ok(x.abs())
    }
}

#[derive(Debug, Coerce, Trace, NoSerialize, AnyLifetime)]
#[repr(C)]
struct PartialGen<V, S> {
    func: V,
    // Always references a tuple.
    pos: V,
    named: Vec<V>,
    names: Vec<(Symbol, S)>,
}

impl<'v, V: ValueLike<'v>, S> PartialGen<V, S> {
    fn pos_content(&self) -> &'v [Value<'v>] {
        Tuple::from_value(self.pos.to_value()).unwrap().content()
    }
}

impl<'v, V: ValueLike<'v>, S> Display for PartialGen<V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "partial({}, *[", self.func)?;
        for (i, v) in self.pos_content().iter().enumerate() {
            if i != 0 {
                write!(f, ",")?;
            }
            v.fmt(f)?;
        }
        write!(f, "], **{{")?;
        for (i, (k, v)) in self.names.iter().zip(self.named.iter()).enumerate() {
            if i != 0 {
                write!(f, ",")?;
            }
            write!(f, "{}:", k.0.as_str())?;
            v.to_value().fmt(f)?;
        }
        write!(f, "}})")
    }
}

type Partial<'v> = PartialGen<Value<'v>, StringValue<'v>>;
type FrozenPartial = PartialGen<FrozenValue, FrozenStringValue>;
starlark_complex_values!(Partial);

impl<'v> Freeze for Partial<'v> {
    type Frozen = FrozenPartial;
    fn freeze(self, freezer: &Freezer) -> anyhow::Result<Self::Frozen> {
        Ok(FrozenPartial {
            func: self.func.freeze(freezer)?,
            pos: freezer.freeze(self.pos)?,
            named: self.named.try_map(|x| x.freeze(freezer))?,
            names: self
                .names
                .into_try_map(|(s, x)| anyhow::Ok((s, x.freeze(freezer)?)))?,
        })
    }
}

impl<'v, V: ValueLike<'v>, S: StringValueLike<'v>> StarlarkValue<'v> for PartialGen<V, S>
where
    Self: AnyLifetime<'v>,
{
    starlark_type!(FUNCTION_TYPE);

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // apply the partial arguments first, then the remaining arguments I was given

        let self_pos = self.pos_content();
        let self_named = coerce_ref(&self.named);
        let self_names = coerce_ref(&self.names);

        eval.alloca_concat(self_pos, args.pos, |pos, eval| {
            eval.alloca_concat(self_named, args.named, |named, eval| {
                eval.alloca_concat(self_names, args.names, |names, eval| {
                    let params = Arguments {
                        pos,
                        named,
                        names,
                        args: args.args,
                        kwargs: args.kwargs,
                    };
                    self.func.invoke(&params, eval)
                })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gazebo::prelude::*;

    use crate::{assert, assert::Assert, stdlib::PrintHandler};

    #[test]
    fn test_filter() {
        assert::pass(
            r#"
def contains_hello(s):
    if "hello" in s:
        return True
    return False

def positive(i):
    return i > 0

assert_eq([], filter(positive, []))
assert_eq([1, 2, 3], filter(positive, [1, 2, 3]))
assert_eq([], filter(positive, [-1, -2, -3]))
assert_eq([1, 2, 3], filter(positive, [-1, 1, 2, -2, -3, 3]))
assert_eq(["hello world!"], filter(contains_hello, ["hello world!", "goodbye"]))
"#,
        );
    }

    #[test]
    fn test_map() {
        assert::pass(
            r#"
def double(x):
    return x + x

assert_eq([], map(int, []))
assert_eq([1,2,3], map(int, ["1","2","3"]))
assert_eq(["0","1","2"], map(str, range(3)))
assert_eq(["11",8], map(double, ["1",4]))
"#,
        );
    }

    #[test]
    fn test_partial() {
        assert::pass(
            r#"
def sum(a, b, *args, **kwargs):
    # print("a=%s b=%s args=%s kwargs=%s" % (a, b, args, kwargs))
    args = (a, b) + args
    return [args, kwargs]

# simple test
assert_eq(
    [(1, 2, 3), {"other": True, "third": None}],
    (partial(sum, 1, other=True))(2, 3, third=None))

# passing *args **kwargs to partial
assert_eq(
    [(1, 2, 3), {"other": True, "third": None}],
    (partial(sum, *[1], **{"other": True}))(2, 3, third=None))

# passing *args **kwargs to returned func
assert_eq(
    [(1, 2, 3), {"other": True, "third": None}],
    (partial(sum, other=True))(*[1, 2, 3], **{"third": None}))

# no args to partial
assert_eq(
    [(1, 2, 3), {"other": True, "third": None}],
    (partial(sum))(1, 2, 3, third=None, **{"other": True}))
"#,
        );
    }

    #[test]
    fn test_debug() {
        assert::pass(
            r#"assert_eq(
                debug([1,2]),
                "Value(ListGen(List { content: Cell { value: ValueType(Value(Array { len: 2, capacity: 2, iter_count: 0, content: [Value(1), Value(2)] })) } }))"
                )"#,
        );
    }

    #[test]
    fn test_dedupe() {
        assert::pass(
            r#"
assert_eq(dedupe([1,2,3]), [1,2,3])
assert_eq(dedupe([1,2,3,2,1]), [1,2,3])
a = [1]
b = [1]
assert_eq(dedupe([a,b,a]), [a,b])
"#,
        );
    }

    #[test]
    fn test_print() {
        let s = Rc::new(RefCell::new(String::new()));
        let s_copy = s.dupe();
        struct PrintHandlerImpl {
            s: Rc<RefCell<String>>,
        }
        impl PrintHandler for PrintHandlerImpl {
            fn println(&self, s: &str) -> anyhow::Result<()> {
                *self.s.borrow_mut() = s.to_owned();
                Ok(())
            }
        }
        let print_handler = PrintHandlerImpl { s: s.dupe() };
        let mut a = Assert::new();
        a.set_print_handler(&print_handler);
        a.pass("print('hw')");
        assert_eq!("hw", s_copy.borrow().as_str());
    }
}
