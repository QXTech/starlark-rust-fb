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
    any::TypeId,
    cmp,
    cmp::Ordering,
    fmt::{Debug, Display},
    mem,
    mem::MaybeUninit,
    ptr,
    ptr::metadata,
};

use derive_more::Display;
use gazebo::{any::AnyLifetime, cast, coerce::Coerce, prelude::*};
use serde::{Serialize, Serializer};

use crate::{
    collections::{StarlarkHashValue, StarlarkHasher},
    environment::Methods,
    eval::{Arguments, Evaluator, FrozenDef},
    values::{
        basic::StarlarkValueBasic,
        bool::StarlarkBool,
        docs::DocItem,
        float::StarlarkFloat,
        layout::arena::{AValueForward, AValueHeader, AValueRepr},
        list::{FrozenList, List, ListGen},
        none::NoneType,
        num::Num,
        string::StarlarkStr,
        types::{
            array::Array,
            tuple::{FrozenTuple, Tuple},
        },
        ComplexValue, Freezer, FrozenStringValue, FrozenValue, Heap, StarlarkValue,
        StarlarkValueDyn, Trace, Tracer, Value, ValueTyped,
    },
};

pub(crate) static VALUE_NONE: AValueRepr<AValueImpl<Basic, NoneType>> = {
    const PAYLOAD: AValueImpl<Basic, NoneType> = AValueImpl(Basic, NoneType);
    const DYN: &dyn AValueDyn<'static> = &PAYLOAD;
    AValueRepr::with_metadata(metadata(DYN), PAYLOAD)
};

pub(crate) static VALUE_FALSE: AValueRepr<AValueImpl<Basic, StarlarkBool>> = {
    const PAYLOAD: AValueImpl<Basic, StarlarkBool> = AValueImpl(Basic, StarlarkBool(false));
    const DYN: &dyn AValueDyn<'static> = &PAYLOAD;
    AValueRepr::with_metadata(metadata(DYN), PAYLOAD)
};

pub(crate) static VALUE_TRUE: AValueRepr<AValueImpl<Basic, StarlarkBool>> = {
    const PAYLOAD: AValueImpl<Basic, StarlarkBool> = AValueImpl(Basic, StarlarkBool(true));
    const DYN: &dyn AValueDyn<'static> = &PAYLOAD;
    AValueRepr::with_metadata(metadata(DYN), PAYLOAD)
};

pub(crate) const VALUE_STR_A_VALUE_PTR: AValueHeader = {
    #[allow(clippy::declare_interior_mutable_const)]
    const VTABLE: AValueImpl<Direct, StarlarkStr> =
        AValueImpl(Direct, unsafe { StarlarkStr::new(0) });
    AValueHeader::with_metadata(metadata(
        &VTABLE as *const AValueImpl<Direct, StarlarkStr> as *const dyn AValueDyn<'static>,
    ))
};

pub(crate) static VALUE_EMPTY_TUPLE: AValueRepr<AValueImpl<Direct, FrozenTuple>> = {
    const PAYLOAD: AValueImpl<Direct, FrozenTuple> =
        AValueImpl(Direct, unsafe { FrozenTuple::new(0) });
    const DYN: &dyn AValueDyn<'static> = &PAYLOAD;
    AValueRepr::with_metadata(metadata(DYN), PAYLOAD)
};

pub(crate) static VALUE_EMPTY_FROZEN_LIST: AValueRepr<AValueImpl<Direct, ListGen<FrozenList>>> = {
    const PAYLOAD: AValueImpl<Direct, ListGen<FrozenList>> =
        AValueImpl(Direct, ListGen(unsafe { FrozenList::new(0) }));
    const DYN: &dyn AValueDyn<'static> = &PAYLOAD;
    AValueRepr::with_metadata(metadata(DYN), PAYLOAD)
};

/// `Array` is not `Sync`, so wrap it into this struct to store it in static variable.
/// Empty `Array` is logically `Sync`.
pub(crate) struct ValueEmptyArray(AValueRepr<AValueImpl<Direct, Array<'static>>>);
unsafe impl Sync for ValueEmptyArray {}

pub(crate) static VALUE_EMPTY_ARRAY: ValueEmptyArray = {
    const DYN: *const dyn AValueDyn<'static> = ptr::null::<AValueImpl<Direct, Array>>();
    ValueEmptyArray(AValueRepr::with_metadata(
        metadata(DYN),
        AValueImpl(Direct, unsafe { Array::new(0, 0) }),
    ))
};

impl ValueEmptyArray {
    pub(crate) fn repr<'v>(
        &'static self,
    ) -> &'v AValueRepr<impl AValue<'v, StarlarkValue = Array<'v>>> {
        // Cast lifetimes. Cannot use `gazebo::cast::ptr_lifetime` here
        // because type parameter of `AValue` also need to be casted.
        unsafe {
            transmute!(
                &AValueRepr<AValueImpl<Direct, Array>>,
                &AValueRepr<AValueImpl<Direct, Array>>,
                &self.0
            )
        }
    }
}

/// Sized counterpart of [`AValueDyn`].
pub(crate) trait AValue<'v>: StarlarkValueDyn<'v> + Sized {
    /// Unwrapped type.
    type StarlarkValue: StarlarkValue<'v>;

    /// Certain types like `Tuple` or `StarlarkStr` have payload array
    /// placed in a heap after `Self`. This is the type of an element of that array.
    type ExtraElem: 'v;

    /// Payload array length.
    fn extra_len(&self) -> usize;

    /// Offset of field holding content, in bytes.
    ///
    /// Return `mem::size_of::<Self>()` if there's no extra content.
    fn offset_of_extra() -> usize;

    fn is_str() -> bool {
        false
    }

    fn memory_size_for_extra_len(extra_len: usize) -> usize {
        assert!(
            Self::offset_of_extra() % mem::align_of::<Self::ExtraElem>() == 0,
            "extra must be aligned"
        );
        cmp::max(
            mem::size_of::<Self::StarlarkValue>(),
            // Content is not necessarily aligned to end of `A`.
            Self::offset_of_extra() + (mem::size_of::<Self::ExtraElem>() * extra_len),
        )
    }

    unsafe fn heap_freeze(
        me: *mut AValueRepr<Self>,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue>;

    unsafe fn heap_copy(me: *mut AValueRepr<Self>, tracer: &Tracer<'v>) -> Value<'v>;

    fn get_hash(&self) -> anyhow::Result<StarlarkHashValue> {
        let mut hasher = StarlarkHasher::new();
        self.write_hash(&mut hasher)?;
        Ok(hasher.finish_small())
    }
}

/// A trait that covers [`StarlarkValue`].
/// If you need a real [`StarlarkValue`] see [`AsStarlarkValue`](crate::values::AsStarlarkValue).
pub(crate) trait AValueDyn<'v>: StarlarkValueDyn<'v> {
    // How much memory I take up on the heap.
    // Included to allow unsized types to live on the heap.
    fn memory_size(&self) -> usize;

    /// This function is not safe because it overwrites `self` value with forward.
    unsafe fn heap_freeze(
        &self,
        me: *mut AValueHeader,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue>;

    /// This function is not safe because it overwrites `self` value with forward.
    unsafe fn heap_copy(&self, me: *mut AValueHeader, tracer: &Tracer<'v>) -> Value<'v>;

    fn is_str(&self) -> bool;

    fn get_hash(&self) -> anyhow::Result<StarlarkHashValue>;
}

impl<'v, A: AValue<'v>> AValueDyn<'v> for A {
    fn memory_size(&self) -> usize {
        Self::memory_size_for_extra_len(self.extra_len())
    }

    unsafe fn heap_freeze(
        &self,
        me: *mut AValueHeader,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        A::heap_freeze((*me).as_repr_mut::<A>(), freezer)
    }

    unsafe fn heap_copy(&self, me: *mut AValueHeader, tracer: &Tracer<'v>) -> Value<'v> {
        A::heap_copy((*me).as_repr_mut::<A>(), tracer)
    }

    fn is_str(&self) -> bool {
        A::is_str()
    }

    fn get_hash(&self) -> anyhow::Result<StarlarkHashValue> {
        self.get_hash()
    }
}

impl<'v> dyn AValueDyn<'v> {
    /// Downcast a reference to type `T`, or return [`None`](None) if it is not the
    /// right type.
    // We'd love to reuse the type from as_dyn_any, but that doesn't seem to have the right vtable-ness
    pub fn downcast_ref<T: StarlarkValue<'v>>(&self) -> Option<&T> {
        if self.static_type_of_value() == T::static_type_id() {
            // SAFETY: just checked whether we are pointing to the correct type.
            unsafe { Some(&*(self as *const Self as *const T)) }
        } else {
            None
        }
    }

    /// Returns the amount of memory a given AValue uses summed with how much it's holding onto.
    /// This is for profiling, this value is not guaranteed to be exact, and might be expensive to
    /// compute.
    pub(crate) fn total_memory(&self) -> usize {
        mem::size_of::<AValueHeader>() + self.memory_size() + self.extra_memory()
    }
}

pub(crate) fn starlark_str(len: usize) -> impl AValue<'static, ExtraElem = u8> + Send + Sync {
    AValueImpl(Direct, unsafe { StarlarkStr::new(len) })
}

pub(crate) fn tuple_avalue<'v>(len: usize) -> impl AValue<'v, ExtraElem = Value<'v>> {
    AValueImpl(Direct, unsafe { Tuple::new(len) })
}

pub(crate) fn frozen_tuple_avalue(len: usize) -> impl AValue<'static, ExtraElem = FrozenValue> {
    AValueImpl(Direct, unsafe { FrozenTuple::new(len) })
}

pub(crate) fn list_avalue<'v>(
    content: ValueTyped<'v, Array<'v>>,
) -> impl AValue<'v, StarlarkValue = ListGen<List<'v>>, ExtraElem = ()> {
    AValueImpl(Direct, ListGen(List::new(content)))
}

pub(crate) fn frozen_list_avalue(len: usize) -> impl AValue<'static, ExtraElem = FrozenValue> {
    AValueImpl(Direct, unsafe { ListGen(FrozenList::new(len)) })
}

pub(crate) fn array_avalue<'v>(
    cap: u32,
) -> impl AValue<'v, StarlarkValue = Array<'v>, ExtraElem = Value<'v>> {
    AValueImpl(Direct, unsafe { Array::new(0, cap) })
}

pub(crate) fn basic_ref<'v, T: StarlarkValueBasic<'v>>(x: &T) -> &dyn AValueDyn<'v> {
    // These are the same representation, so safe to convert
    let x: &AValueImpl<Basic, T> = unsafe { cast::ptr(x) };
    x
}

pub(crate) fn simple<T: StarlarkValue<'static>>(x: T) -> impl AValue<'static, ExtraElem = ()> {
    assert!(!T::is_special());
    AValueImpl(Simple, x)
}

pub(crate) fn complex<'v, C>(x: C) -> impl AValue<'v, ExtraElem = ()>
where
    C: ComplexValue<'v>,
    C::Frozen: StarlarkValue<'static>,
{
    assert!(!C::is_special());
    AValueImpl(Complex, x)
}

pub(crate) fn float_avalue<'v>(x: StarlarkFloat) -> impl AValue<'v, ExtraElem = ()> {
    AValueImpl(Direct, x)
}

// A type where the second element is in control of what instances are in scope
pub(crate) struct Direct;

// A type that implements StarlarkValue but nothing else, so will never be stored
// in the heap (e.g. bool, None)
pub(crate) struct Basic;

// A non-special type with no references to other Starlark values.
pub(crate) struct Simple;

// A type that implements ComplexValue.
pub(crate) struct Complex;

// We want to define several types (Simple, Complex) that wrap a StarlarkValue,
// reimplement it, and do some things custom. The easiest way to avoid repeating
// the StarlarkValue trait each time is to make them all share a single wrapper,
// where Mode is one of Simple/Complex.
#[repr(C)]
pub(crate) struct AValueImpl<Mode, T>(Mode, pub(crate) T);

// Safe because Simple/Complex are ZST
unsafe impl<T> Coerce<T> for AValueImpl<Simple, T> {}
unsafe impl<T> Coerce<T> for AValueImpl<Complex, T> {}

/// The overwrite operation in the heap requires that the LSB not be set.
/// For FrozenValue this is the case, but for Value the LSB is always set.
/// Fortunately, the consumer of the overwritten value reapplies the
/// FrozenValue/Value tags, so we can freely discard it here.
fn clear_lsb(x: usize) -> usize {
    x & !1
}

impl<'v, T: StarlarkValueBasic<'v>> AValue<'v> for AValueImpl<Basic, T> {
    type StarlarkValue = T;

    type ExtraElem = ();

    fn extra_len(&self) -> usize {
        0
    }

    fn offset_of_extra() -> usize {
        mem::size_of::<Self>()
    }

    unsafe fn heap_freeze(
        _me: *mut AValueRepr<Self>,
        _freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        unreachable!("Basic types don't appear in the heap")
    }
    unsafe fn heap_copy(_me: *mut AValueRepr<Self>, _tracer: &Tracer<'v>) -> Value<'v> {
        unreachable!("Basic types don't appear in the heap")
    }

    fn get_hash(&self) -> anyhow::Result<StarlarkHashValue> {
        Ok(self.1.get_hash())
    }
}

impl<'v> AValue<'v> for AValueImpl<Direct, StarlarkFloat> {
    type StarlarkValue = StarlarkFloat;

    type ExtraElem = ();

    fn extra_len(&self) -> usize {
        0
    }

    fn offset_of_extra() -> usize {
        mem::size_of::<Self>()
    }

    unsafe fn heap_freeze(
        me: *mut AValueRepr<Self>,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        Self::heap_freeze_simple_impl(me, freezer)
    }

    unsafe fn heap_copy(me: *mut AValueRepr<Self>, tracer: &Tracer<'v>) -> Value<'v> {
        Self::heap_copy_impl(me, tracer, |_v, _tracer| {})
    }

    fn get_hash(&self) -> anyhow::Result<StarlarkHashValue> {
        Ok(Num::from(self.1.0).get_hash())
    }
}

pub(crate) type StarlarkStrAValue = AValueImpl<Direct, StarlarkStr>;

impl<'v> AValue<'v> for AValueImpl<Direct, StarlarkStr> {
    type StarlarkValue = StarlarkStr;

    type ExtraElem = u8;

    fn extra_len(&self) -> usize {
        self.1.len()
    }

    fn offset_of_extra() -> usize {
        StarlarkStr::offset_of_content()
    }

    fn is_str() -> bool {
        true
    }

    unsafe fn heap_freeze(
        me: *mut AValueRepr<Self>,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        debug_assert!(
            (*me).payload.1.len() > 1,
            "short strings are allocated statically"
        );

        let s = (*me).payload.1.unpack();
        let fv = freezer.alloc(s);
        debug_assert!(fv.is_str());
        AValueHeader::overwrite_with_forward::<Self>(me, fv.0.ptr_value());
        Ok(fv)
    }

    unsafe fn heap_copy(me: *mut AValueRepr<Self>, tracer: &Tracer<'v>) -> Value<'v> {
        debug_assert!(
            (*me).payload.1.len() > 1,
            "short strings are allocated statically"
        );

        let s = (*me).payload.1.unpack();
        let v = tracer.alloc_str(s);
        debug_assert!(v.is_str());
        AValueHeader::overwrite_with_forward::<Self>(me, v.0.ptr_value() & !1);
        v
    }

    fn get_hash(&self) -> anyhow::Result<StarlarkHashValue> {
        Ok(self.1.get_hash())
    }
}

impl<'v> AValue<'v> for AValueImpl<Direct, Tuple<'v>> {
    type StarlarkValue = Tuple<'v>;

    type ExtraElem = Value<'v>;

    fn extra_len(&self) -> usize {
        self.1.len()
    }

    fn offset_of_extra() -> usize {
        Tuple::offset_of_content()
    }

    unsafe fn heap_freeze(
        me: *mut AValueRepr<Self>,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        debug_assert!(
            (*me).payload.1.len() != 0,
            "empty tuple is allocated statically"
        );

        AValueForward::assert_does_not_overwrite_extra::<Self>();
        let content = (*me).payload.1.content();

        let (fv, r, extra) =
            freezer.reserve_with_extra::<AValueImpl<Direct, FrozenTuple>>(content.len());
        AValueHeader::overwrite_with_forward::<Self>(me, fv.0.ptr_value());

        // TODO: this allocation is unnecessary
        let frozen_values = content.try_map(|v| freezer.freeze(*v))?;
        r.fill(AValueImpl(Direct, FrozenTuple::new(content.len())));
        MaybeUninit::write_slice(extra, &frozen_values);

        Ok(fv)
    }

    unsafe fn heap_copy(me: *mut AValueRepr<Self>, tracer: &Tracer<'v>) -> Value<'v> {
        debug_assert!(
            (*me).payload.1.len() != 0,
            "empty tuple is allocated statically"
        );

        AValueForward::assert_does_not_overwrite_extra::<Self>();
        let content = (*me).payload.1.content_mut();

        let (v, r, extra) = tracer.reserve_with_extra::<Self>(content.len());
        let x = AValueHeader::overwrite_with_forward::<Self>(me, clear_lsb(v.0.ptr_value()));

        debug_assert_eq!(content.len(), x.1.len());

        for elem in content.iter_mut() {
            tracer.trace(elem);
        }
        r.fill(x);
        MaybeUninit::write_slice(extra, content);
        v
    }
}

impl<'v> AValue<'v> for AValueImpl<Direct, FrozenTuple> {
    type StarlarkValue = FrozenTuple;

    type ExtraElem = FrozenValue;

    fn extra_len(&self) -> usize {
        self.1.len()
    }

    fn offset_of_extra() -> usize {
        FrozenTuple::offset_of_content()
    }

    unsafe fn heap_freeze(
        _me: *mut AValueRepr<Self>,
        _freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        panic!("already frozen");
    }

    unsafe fn heap_copy(_me: *mut AValueRepr<Self>, _tracer: &Tracer<'v>) -> Value<'v> {
        panic!("shouldn't be copying frozen values");
    }
}

impl<'v> AValue<'v> for AValueImpl<Direct, ListGen<List<'v>>> {
    type StarlarkValue = ListGen<List<'v>>;

    type ExtraElem = ();

    fn extra_len(&self) -> usize {
        0
    }

    fn offset_of_extra() -> usize {
        mem::size_of::<Self>()
    }

    unsafe fn heap_freeze(
        me: *mut AValueRepr<Self>,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        let content = (*me).payload.1.0.content();

        if content.is_empty() {
            let fv = FrozenValue::new_repr(&VALUE_EMPTY_FROZEN_LIST);
            AValueHeader::overwrite_with_forward::<Self>(me, fv.0.ptr_value());
            return Ok(fv);
        }

        let (fv, r, extra) =
            freezer.reserve_with_extra::<AValueImpl<Direct, ListGen<FrozenList>>>(content.len());
        AValueHeader::overwrite_with_forward::<Self>(me, fv.0.ptr_value());
        r.fill(AValueImpl(Direct, ListGen(FrozenList::new(content.len()))));
        assert_eq!(extra.len(), content.len());
        for (elem_place, elem) in extra.iter_mut().zip(content) {
            elem_place.write(freezer.freeze(*elem)?);
        }
        Ok(fv)
    }

    unsafe fn heap_copy(me: *mut AValueRepr<Self>, tracer: &Tracer<'v>) -> Value<'v> {
        Self::heap_copy_impl(me, tracer, Trace::trace)
    }
}

impl<'v> AValue<'v> for AValueImpl<Direct, ListGen<FrozenList>> {
    type StarlarkValue = ListGen<FrozenList>;

    type ExtraElem = FrozenValue;

    fn extra_len(&self) -> usize {
        self.1.0.len()
    }

    fn offset_of_extra() -> usize {
        ListGen::<FrozenList>::offset_of_content()
    }

    unsafe fn heap_freeze(
        _me: *mut AValueRepr<Self>,
        _freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        panic!("already frozen");
    }

    unsafe fn heap_copy(_me: *mut AValueRepr<Self>, _tracer: &Tracer<'v>) -> Value<'v> {
        panic!("shouldn't be copying frozen values");
    }
}

impl<'v> AValue<'v> for AValueImpl<Direct, Array<'v>> {
    type StarlarkValue = Array<'v>;

    type ExtraElem = Value<'v>;

    fn extra_len(&self) -> usize {
        // Note we return capacity, not length here.
        self.1.capacity()
    }

    fn offset_of_extra() -> usize {
        Array::offset_of_content()
    }

    unsafe fn heap_freeze(
        _me: *mut AValueRepr<Self>,
        _freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        panic!("arrays should not be frozen")
    }

    unsafe fn heap_copy(me: *mut AValueRepr<Self>, tracer: &Tracer<'v>) -> Value<'v> {
        debug_assert!(
            (*me).payload.1.capacity() != 0,
            "empty array is allocated statically"
        );

        if (*me).payload.1.len() == 0 {
            return FrozenValue::new_repr(&VALUE_EMPTY_ARRAY.0).to_value();
        }

        AValueForward::assert_does_not_overwrite_extra::<Self>();
        let content = (*me).payload.1.content_mut();

        let (v, r, extra) = tracer.reserve_with_extra::<Self>(content.len());
        let x = AValueHeader::overwrite_with_forward::<Self>(me, clear_lsb(v.0.ptr_value()));

        debug_assert_eq!(content.len(), x.1.len());

        content.trace(tracer);

        // Note when copying we are dropping extra capacity.
        r.fill(AValueImpl(
            Direct,
            Array::new(content.len() as u32, content.len() as u32),
        ));
        MaybeUninit::write_slice(extra, content);
        v
    }
}

impl<Mode, C> AValueImpl<Mode, C> {
    /// `heap_freeze` implementation for simple `StarlarkValue` and `StarlarkFloat`
    /// (`StarlarkFloat` is logically a simple type, but it is not considered simple type).
    unsafe fn heap_freeze_simple_impl<'v>(
        me: *mut AValueRepr<Self>,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue>
    where
        Self: AValue<'v, ExtraElem = ()>,
    {
        let (fv, r) = freezer.reserve::<Self>();
        let x = AValueHeader::overwrite_with_forward::<Self>(me, fv.0.ptr_value());
        r.fill(x);
        Ok(fv)
    }
}

impl<'v, T: StarlarkValue<'static>> AValue<'v> for AValueImpl<Simple, T>
where
    'v: 'static,
{
    type StarlarkValue = T;

    type ExtraElem = ();

    fn extra_len(&self) -> usize {
        0
    }

    fn offset_of_extra() -> usize {
        mem::size_of::<Self>()
    }

    unsafe fn heap_freeze(
        me: *mut AValueRepr<Self>,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        Self::heap_freeze_simple_impl(me, freezer)
    }

    unsafe fn heap_copy(me: *mut AValueRepr<Self>, tracer: &Tracer<'v>) -> Value<'v> {
        Self::heap_copy_impl(me, tracer, |_v, _tracer| {})
    }
}

impl<Mode, C> AValueImpl<Mode, C> {
    /// Common `heap_copy` implementation for types without extra.
    unsafe fn heap_copy_impl<'v>(
        me: *mut AValueRepr<Self>,
        tracer: &Tracer<'v>,
        trace: impl FnOnce(&mut C, &Tracer<'v>),
    ) -> Value<'v>
    where
        Self: AValue<'v, ExtraElem = ()>,
    {
        let (v, r) = tracer.reserve::<Self>();
        let mut x = AValueHeader::overwrite_with_forward::<Self>(me, clear_lsb(v.0.ptr_value()));
        // We have to put the forwarding node in _before_ we trace in case there are cycles
        trace(&mut x.1, tracer);
        r.fill(x);
        v
    }
}

impl<'v, T> AValue<'v> for AValueImpl<Complex, T>
where
    T: ComplexValue<'v>,
    T::Frozen: StarlarkValue<'static>,
{
    type StarlarkValue = T;

    type ExtraElem = ();

    fn extra_len(&self) -> usize {
        0
    }

    fn offset_of_extra() -> usize {
        mem::size_of::<Self>()
    }

    unsafe fn heap_freeze(
        me: *mut AValueRepr<Self>,
        freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        let (fv, r) = freezer.reserve::<AValueImpl<Simple, T::Frozen>>();
        let x = AValueHeader::overwrite_with_forward::<Self>(me, fv.0.ptr_value());
        let res = x.1.freeze(freezer)?;
        r.fill(AValueImpl(Simple, res));
        if TypeId::of::<T::Frozen>() == TypeId::of::<FrozenDef>() {
            let frozen_def = fv.downcast_frozen_ref().unwrap();
            freezer.frozen_defs.borrow_mut().push(frozen_def);
        }
        Ok(fv)
    }

    unsafe fn heap_copy(me: *mut AValueRepr<Self>, tracer: &Tracer<'v>) -> Value<'v> {
        Self::heap_copy_impl(me, tracer, Trace::trace)
    }
}

#[derive(Debug, Display)]
#[display(fmt = "BlackHole")]
pub(crate) struct BlackHole(pub(crate) usize);

impl<'v> AValueDyn<'v> for BlackHole {
    fn memory_size(&self) -> usize {
        self.0
    }

    unsafe fn heap_freeze(
        &self,
        _me: *mut AValueHeader,
        _freezer: &Freezer,
    ) -> anyhow::Result<FrozenValue> {
        unreachable!()
    }
    unsafe fn heap_copy(&self, _me: *mut AValueHeader, _tracer: &Tracer<'v>) -> Value<'v> {
        unreachable!()
    }

    fn is_str(&self) -> bool {
        // We don't create reservations for `StarlarkStr`.
        false
    }

    fn get_hash(&self) -> anyhow::Result<StarlarkHashValue> {
        unreachable!()
    }
}

impl<'v> StarlarkValueDyn<'v> for BlackHole {
    fn static_type_id_of_value() -> TypeId
    where
        Self: Sized,
    {
        panic!()
    }

    fn static_type_of_value(&self) -> TypeId {
        panic!()
    }

    fn as_debug(&self) -> &dyn Debug {
        self
    }

    fn as_display(&self) -> &dyn Display {
        self
    }

    fn value_as_dyn_any(&self) -> &dyn AnyLifetime<'v> {
        panic!()
    }

    // The remaining operations are implementations of starlark operations,
    // all of them panic because they are not supposed to be called on `BlackHole`.

    fn get_type(&self) -> &'static str {
        panic!()
    }
    fn get_type_value(&self) -> FrozenStringValue {
        panic!()
    }
    fn matches_type(&self, _ty: &str) -> bool {
        panic!()
    }
    fn get_methods(&self) -> Option<&'static Methods> {
        panic!()
    }
    fn documentation(&self) -> Option<DocItem> {
        panic!()
    }
    fn collect_repr(&self, _collector: &mut String) {
        panic!()
    }
    fn collect_repr_cycle(&self, _collector: &mut String) {
        panic!()
    }
    fn to_bool(&self) -> bool {
        panic!()
    }
    fn to_int(&self) -> anyhow::Result<i32> {
        panic!()
    }
    fn write_hash(&self, _hasher: &mut StarlarkHasher) -> anyhow::Result<()> {
        panic!()
    }
    fn extra_memory(&self) -> usize {
        panic!()
    }
    fn equals(&self, _other: Value<'v>) -> anyhow::Result<bool> {
        panic!()
    }
    fn compare(&self, _other: Value<'v>) -> anyhow::Result<Ordering> {
        panic!()
    }
    fn invoke(
        &self,
        _me: Value<'v>,
        _args: &Arguments<'v, '_>,
        _eval: &mut Evaluator<'v, '_>,
    ) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn invoke_method(
        &self,
        _me: Value<'v>,
        _this: Value<'v>,
        _args: &Arguments<'v, '_>,
        _eval: &mut Evaluator<'v, '_>,
    ) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn at(&self, _index: Value<'v>, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn slice(
        &self,
        _start: Option<Value<'v>>,
        _stop: Option<Value<'v>>,
        _stride: Option<Value<'v>>,
        _heap: &'v Heap,
    ) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn iterate<'a>(
        &'a self,
        _heap: &'v Heap,
    ) -> anyhow::Result<Box<dyn Iterator<Item = Value<'v>> + 'a>>
    where
        'v: 'a,
    {
        panic!()
    }
    fn with_iterator(
        &self,
        _heap: &'v Heap,
        _f: &mut dyn FnMut(&mut dyn Iterator<Item = Value<'v>>) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        panic!()
    }
    fn length(&self) -> anyhow::Result<i32> {
        panic!()
    }
    fn get_attr(&self, _attribute: &str, _heap: &'v Heap) -> Option<Value<'v>> {
        panic!()
    }
    fn has_attr(&self, _attribute: &str) -> bool {
        panic!()
    }
    fn dir_attr(&self) -> Vec<String> {
        panic!()
    }
    fn is_in(&self, _other: Value<'v>) -> anyhow::Result<bool> {
        panic!()
    }
    fn plus(&self, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn minus(&self, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn radd(&self, _lhs: Value<'v>, _heap: &'v Heap) -> Option<anyhow::Result<Value<'v>>> {
        panic!()
    }
    fn add(&self, _rhs: Value<'v>, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn sub(&self, _other: Value<'v>, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn mul(&self, _other: Value<'v>, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn percent(&self, _other: Value<'v>, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn div(&self, _other: Value<'v>, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn floor_div(&self, _other: Value<'v>, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn bit_and(&self, _other: Value<'v>) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn bit_or(&self, _other: Value<'v>, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn bit_xor(&self, _other: Value<'v>) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn left_shift(&self, _other: Value<'v>) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn right_shift(&self, _other: Value<'v>) -> anyhow::Result<Value<'v>> {
        panic!()
    }
    fn export_as(&self, _variable_name: &str, _eval: &mut Evaluator<'v, '_>) {
        panic!()
    }
    fn set_at(&self, _index: Value<'v>, _new_value: Value<'v>) -> anyhow::Result<()> {
        panic!()
    }
    fn set_attr(&self, _attribute: &str, _new_value: Value<'v>) -> anyhow::Result<()> {
        panic!()
    }
}

impl Serialize for BlackHole {
    fn serialize<S>(&self, _s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        panic!()
    }
}

impl<'v, Mode: 'static, T: StarlarkValue<'v>> StarlarkValueDyn<'v> for AValueImpl<Mode, T> {
    fn static_type_id_of_value() -> TypeId
    where
        Self: Sized,
    {
        T::static_type_id()
    }
    fn static_type_of_value(&self) -> TypeId {
        T::static_type_id()
    }
    fn as_debug(&self) -> &dyn Debug {
        &self.1
    }
    fn as_display(&self) -> &dyn Display {
        &self.1
    }
    fn value_as_dyn_any(&self) -> &dyn AnyLifetime<'v> {
        &self.1
    }

    // Following operations delegate to `StarlarkValue`.

    fn get_type(&self) -> &'static str {
        self.1.get_type()
    }
    fn get_type_value(&self) -> FrozenStringValue {
        T::get_type_value_static()
    }
    fn matches_type(&self, ty: &str) -> bool {
        self.1.matches_type(ty)
    }
    fn get_methods(&self) -> Option<&'static Methods> {
        self.1.get_methods()
    }
    fn documentation(&self) -> Option<DocItem> {
        self.1.documentation()
    }
    fn collect_repr(&self, collector: &mut String) {
        self.1.collect_repr(collector)
    }
    fn collect_repr_cycle(&self, collector: &mut String) {
        self.1.collect_repr_cycle(collector)
    }
    fn to_bool(&self) -> bool {
        self.1.to_bool()
    }
    fn to_int(&self) -> anyhow::Result<i32> {
        self.1.to_int()
    }
    fn write_hash(&self, hasher: &mut StarlarkHasher) -> anyhow::Result<()> {
        self.1.write_hash(hasher)
    }
    fn extra_memory(&self) -> usize {
        self.1.extra_memory()
    }
    fn equals(&self, other: Value<'v>) -> anyhow::Result<bool> {
        self.1.equals(other)
    }
    fn compare(&self, other: Value<'v>) -> anyhow::Result<Ordering> {
        self.1.compare(other)
    }
    fn invoke(
        &self,
        me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_>,
    ) -> anyhow::Result<Value<'v>> {
        self.1.invoke(me, args, eval)
    }
    fn invoke_method(
        &self,
        me: Value<'v>,
        this: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_>,
    ) -> anyhow::Result<Value<'v>> {
        self.1.invoke_method(me, this, args, eval)
    }
    fn at(&self, index: Value<'v>, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.at(index, heap)
    }
    fn slice(
        &self,
        start: Option<Value<'v>>,
        stop: Option<Value<'v>>,
        stride: Option<Value<'v>>,
        heap: &'v Heap,
    ) -> anyhow::Result<Value<'v>> {
        self.1.slice(start, stop, stride, heap)
    }
    fn iterate<'a>(
        &'a self,
        heap: &'v Heap,
    ) -> anyhow::Result<Box<dyn Iterator<Item = Value<'v>> + 'a>>
    where
        'v: 'a,
    {
        self.1.iterate(heap)
    }
    fn with_iterator(
        &self,
        heap: &'v Heap,
        f: &mut dyn FnMut(&mut dyn Iterator<Item = Value<'v>>) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        self.1.with_iterator(heap, f)
    }
    fn length(&self) -> anyhow::Result<i32> {
        self.1.length()
    }
    fn get_attr(&self, attribute: &str, heap: &'v Heap) -> Option<Value<'v>> {
        self.1.get_attr(attribute, heap)
    }
    fn has_attr(&self, attribute: &str) -> bool {
        self.1.has_attr(attribute)
    }
    fn dir_attr(&self) -> Vec<String> {
        self.1.dir_attr()
    }
    fn is_in(&self, other: Value<'v>) -> anyhow::Result<bool> {
        self.1.is_in(other)
    }
    fn plus(&self, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.plus(heap)
    }
    fn minus(&self, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.minus(heap)
    }
    fn radd(&self, lhs: Value<'v>, heap: &'v Heap) -> Option<anyhow::Result<Value<'v>>> {
        self.1.radd(lhs, heap)
    }
    fn add(&self, rhs: Value<'v>, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.add(rhs, heap)
    }
    fn sub(&self, other: Value<'v>, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.sub(other, heap)
    }
    fn mul(&self, other: Value<'v>, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.mul(other, heap)
    }
    fn percent(&self, other: Value<'v>, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.percent(other, heap)
    }
    fn div(&self, other: Value<'v>, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.div(other, heap)
    }
    fn floor_div(&self, other: Value<'v>, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.floor_div(other, heap)
    }
    fn bit_and(&self, other: Value<'v>) -> anyhow::Result<Value<'v>> {
        self.1.bit_and(other)
    }
    fn bit_or(&self, other: Value<'v>, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        self.1.bit_or(other, heap)
    }
    fn bit_xor(&self, other: Value<'v>) -> anyhow::Result<Value<'v>> {
        self.1.bit_xor(other)
    }
    fn left_shift(&self, other: Value<'v>) -> anyhow::Result<Value<'v>> {
        self.1.left_shift(other)
    }
    fn right_shift(&self, other: Value<'v>) -> anyhow::Result<Value<'v>> {
        self.1.right_shift(other)
    }
    fn export_as(&self, variable_name: &str, eval: &mut Evaluator<'v, '_>) {
        self.1.export_as(variable_name, eval)
    }
    fn set_at(&self, index: Value<'v>, new_value: Value<'v>) -> anyhow::Result<()> {
        self.1.set_at(index, new_value)
    }
    fn set_attr(&self, attribute: &str, new_value: Value<'v>) -> anyhow::Result<()> {
        self.1.set_attr(attribute, new_value)
    }
}

impl<'v, Mode: 'static, T: StarlarkValue<'v>> Serialize for AValueImpl<Mode, T> {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        erased_serde::serialize(&self.1, s)
    }
}

#[cfg(test)]
mod tests {
    use crate::{environment::Module, values::list::List};

    #[test]
    fn tuple_cycle_freeze() {
        let module = Module::new();
        let list = module.heap().alloc_list(&[]);
        let tuple = module.heap().alloc_tuple(&[list]);
        List::from_value_mut(list)
            .unwrap()
            .unwrap()
            .push(tuple, module.heap());
        module.set("t", tuple);
        module.freeze().unwrap();
    }
}
