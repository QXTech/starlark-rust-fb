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

//! Helpers for numerical values.

use either::Either;
use gazebo::prelude::*;

use crate::{
    collections::StarlarkHashValue,
    values::{types::float::StarlarkFloat, UnpackValue, Value},
};

/// [`Num`] represents a numerical value that can be unpacked from a [`Value`].
///
/// It's an intermediate representation that facilitates conversions between
/// numerical types and helps in implementation of arithmetical operations
/// between them.
#[derive(Clone, Copy, Dupe, Debug)]
pub enum Num {
    Int(i32),
    Float(f64),
}

type UnpackNumImpl = Either<i32, StarlarkFloat>;

impl<'v> UnpackValue<'v> for Num {
    fn expected() -> String {
        UnpackNumImpl::expected()
    }

    fn unpack_value(value: Value<'v>) -> Option<Self> {
        Some(match UnpackNumImpl::unpack_value(value)? {
            Either::Left(i) => Num::Int(i),
            Either::Right(f) => Num::Float(f.0),
        })
    }
}

impl Num {
    /// Get underlying value as float
    pub fn as_float(self) -> f64 {
        match self {
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
        }
    }

    /// Get underlying value as int (if it can be precisely expressed as int)
    pub fn as_int(self) -> Option<i32> {
        match self {
            Self::Int(i) => Some(i),
            Self::Float(f) => {
                // f64 can precisely represent all i32 values
                // by a simple cast here we get for free:
                // - making sure f doesn't have fractional part
                // - i32 boundary checks
                // - handling of special floats (+/- inf, nan)
                let int_candidate = f as i32;
                if f == int_candidate as f64 {
                    Some(int_candidate)
                } else {
                    None
                }
            }
        }
    }

    /// Get hash of the underlying number
    pub(crate) fn get_hash_64(self) -> u64 {
        match (self.as_int(), self) {
            // equal ints and floats should have the same hash
            (Some(i), _) => i as u64,
            (None, Self::Float(f)) => {
                if f.is_nan() {
                    // all possible NaNs should hash to the same value
                    0
                } else if f.is_infinite() {
                    u64::MAX
                } else if f == 0.0 {
                    // Both 0.0 and -0.0 need the same hash, but are both equal to 0.0
                    0.0f64.to_bits()
                } else {
                    f.to_bits()
                }
            }
            (None, Self::Int(i)) => {
                // shouldn't happen - as_int() should have resulted in an int
                i as u64
            }
        }
    }

    pub(crate) fn get_hash(self) -> StarlarkHashValue {
        StarlarkHashValue::hash_64(self.get_hash_64())
    }
}

impl From<i32> for Num {
    fn from(i: i32) -> Self {
        Self::Int(i)
    }
}

impl From<f64> for Num {
    fn from(f: f64) -> Self {
        Self::Float(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_value() {
        assert!(Num::unpack_value(Value::new_bool(true)).is_none());
        assert!(Num::unpack_value(Value::new_bool(false)).is_none());
        assert!(Num::unpack_value(Value::new_empty_string()).is_none());
        assert!(Num::unpack_value(Value::new_none()).is_none());

        assert_eq!(
            Num::unpack_value(Value::new_int(0)).unwrap().as_int(),
            Some(0)
        );
        assert_eq!(
            Num::unpack_value(Value::new_int(42)).unwrap().as_int(),
            Some(42)
        );
        assert_eq!(
            Num::unpack_value(Value::new_int(-42)).unwrap().as_int(),
            Some(-42)
        );
    }

    #[test]
    fn test_converstion_to_float() {
        assert_eq!(Num::Int(0).as_float(), 0.0);
        assert_eq!(Num::Int(i32::MAX).as_float(), i32::MAX as f64);
        assert_eq!(Num::Int(i32::MIN).as_float(), i32::MIN as f64);

        assert_eq!(Num::Float(0.0).as_float(), 0.0);
        assert!(Num::Float(f64::NAN).as_float().is_nan());
    }

    #[test]
    fn test_conversion_to_int() {
        assert_eq!(Num::Int(0).as_int(), Some(0));
        assert_eq!(Num::Int(42).as_int(), Some(42));
        assert_eq!(Num::Int(-42).as_int(), Some(-42));

        assert_eq!(Num::Float(0_f64).as_int(), Some(0));
        assert_eq!(Num::Float(42_f64).as_int(), Some(42));
        assert_eq!(Num::Float(-42_f64).as_int(), Some(-42));

        assert_eq!(Num::Float(i32::MIN as f64).as_int(), Some(i32::MIN));
        assert_eq!(Num::Float(i32::MAX as f64).as_int(), Some(i32::MAX));

        assert_eq!(Num::Float(42.75).as_int(), None);
        assert_eq!(Num::Float(-42.75).as_int(), None);
        assert_eq!(Num::Float(f64::NAN).as_int(), None);
        assert_eq!(Num::Float(f64::INFINITY).as_int(), None);
        assert_eq!(Num::Float(f64::NEG_INFINITY).as_int(), None);
    }

    #[test]
    fn test_hashing() {
        assert_eq!(Num::Int(0).get_hash_64(), Num::Float(0.0).get_hash_64());
        assert_eq!(Num::Int(42).get_hash_64(), Num::Float(42.0).get_hash_64());

        assert_eq!(
            Num::Float(f64::INFINITY + f64::NEG_INFINITY).get_hash_64(),
            Num::Float(f64::NAN).get_hash_64()
        );
        assert_eq!(
            Num::Float("0.25".parse().unwrap()).get_hash_64(),
            Num::Float("25e-2".parse().unwrap()).get_hash_64()
        );
    }
}
