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

use crate::values::{Freezer, FrozenValue, Value};
use gazebo::prelude::*;
use std::cell::{RefCell, RefMut};

#[derive(Clone, Copy, Dupe, Debug, PartialEq, Eq)]
pub(crate) struct ModuleSlotId(usize);

impl ModuleSlotId {
    pub fn new(index: usize) -> Self {
        Self(index)
    }
}

// Indexed slots of a module. May contain unassigned values
#[derive(Debug)]
pub(crate) struct MutableSlots<'v>(RefCell<Vec<Value<'v>>>);

// Indexed slots of a module. May contain unassigned values
#[derive(Debug)]
pub(crate) struct FrozenSlots(Vec<FrozenValue>);

impl<'v> MutableSlots<'v> {
    pub fn new() -> Self {
        Self(RefCell::new(Vec::new()))
    }

    pub(crate) fn get_slots_mut(&self) -> RefMut<Vec<Value<'v>>> {
        self.0.borrow_mut()
    }

    pub fn get_slot(&self, slot: ModuleSlotId) -> Option<Value<'v>> {
        let v = self.0.borrow()[slot.0];
        if v.is_unassigned() { None } else { Some(v) }
    }

    pub fn set_slot(&self, slot: ModuleSlotId, value: Value<'v>) {
        assert!(!value.is_unassigned());
        self.0.borrow_mut()[slot.0] = value;
    }

    pub fn ensure_slot(&self, slot: ModuleSlotId) {
        // To ensure that `slot` exists, we need at least `slot + 1` slots.
        self.ensure_slots(slot.0 + 1);
    }

    pub fn ensure_slots(&self, count: usize) {
        let mut slots = self.0.borrow_mut();
        if slots.len() >= count {
            return;
        }
        let extra = count - slots.len();
        slots.reserve(extra);
        for _ in 0..extra {
            slots.push(Value::new_unassigned());
        }
    }

    pub(crate) fn freeze(self, freezer: &Freezer) -> FrozenSlots {
        let slots = self.0.into_inner().map(|x| x.freeze(freezer));
        FrozenSlots(slots)
    }
}

impl FrozenSlots {
    pub fn get_slot(&self, slot: ModuleSlotId) -> Option<FrozenValue> {
        let fv = self.0[slot.0];
        if fv.is_unassigned() { None } else { Some(fv) }
    }
}
