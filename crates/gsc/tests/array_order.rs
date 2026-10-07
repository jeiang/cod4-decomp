// SPDX-License-Identifier: GPL-3.0-or-later
//! Pins the element order of script arrays to the original's.
//!
//! Evidence (KisakCOD `script/scr_variable.cpp`, same code as iw3mp.exe 1.7):
//! - `Scr_AddArrayKeys` walks `FindFirstSibling` then `FindNextSibling` over the array
//!   object's child list and appends each key to the result.
//! - Every ordinary store (`GetNewArrayVariableIndex`, `GetNewVariableIndexInternal2`) links
//!   the new child in at the head of that list (`parentValue->nextSibling = id`); only
//!   `GetNewVariableIndexReverseInternal2` appends at the tail and arrays do not use it.
//! - `RemoveVariable` unlinks in place.
//! - `CopyArray` (copy on write of a shared array) walks the source list and head-inserts
//!   into the copy through `GetVariableIndexInternal`, reversing the order.
//!
//! So `getarraykeys` returns keys newest first, and copying flips that.

use gsc::{Array, Key};

fn s(n: &str) -> Key {
    Key::Str(n.into())
}

fn keys(a: &Array) -> Vec<Key> {
    a.keys_original_order().cloned().collect()
}

fn ints(it: impl IntoIterator<Item = i32>) -> Vec<Key> {
    it.into_iter().map(Key::Int).collect()
}

#[test]
fn appended_arrays_list_newest_first() {
    let mut a = Array::new();
    for i in 0..4 {
        a.set(Key::Int(i), gsc::Value::Int(i * 10));
    }
    assert_eq!(keys(&a), ints([3, 2, 1, 0]));
    // Indexed access is unaffected: `for (i = 0; i < a.size; i++) a[i]` sees 0..n in order.
    assert_eq!(a.len(), 4);
    assert!(matches!(a.get(&Key::Int(1)), Some(gsc::Value::Int(10))));
}

#[test]
fn string_and_mixed_keys_follow_insertion_too() {
    let mut a = Array::new();
    a.set(s("b"), gsc::Value::Int(1));
    a.set(Key::Int(0), gsc::Value::Int(2));
    a.set(s("a"), gsc::Value::Int(3));
    assert_eq!(keys(&a), [s("a"), Key::Int(0), s("b")]);
}

#[test]
fn overwrite_keeps_place_and_removal_unlinks_in_place() {
    let mut a = Array::new();
    for i in 0..4 {
        a.set(Key::Int(i), gsc::Value::Int(i));
    }
    a.set(Key::Int(1), gsc::Value::Int(99));
    assert_eq!(keys(&a), ints([3, 2, 1, 0]));
    a.set(Key::Int(2), gsc::Value::Undefined);
    assert_eq!(keys(&a), ints([3, 1, 0]));
    // Re-adding a removed key links it in at the head again.
    a.set(Key::Int(2), gsc::Value::Int(5));
    assert_eq!(keys(&a), ints([2, 3, 1, 0]));
    assert_eq!(a.len(), 4);
}

#[test]
fn copy_on_write_reverses_the_order() {
    let mut a = Array::new();
    for i in 0..3 {
        a.set(Key::Int(i), gsc::Value::Int(i));
    }
    let mut b = a.clone();
    assert_eq!(keys(&a), ints([2, 1, 0]));
    assert_eq!(keys(&b), ints([0, 1, 2]));
    b.set(Key::Int(3), gsc::Value::Int(3));
    assert_eq!(keys(&b), ints([3, 0, 1, 2]));
    // A second copy flips it back.
    assert_eq!(keys(&b.clone()), ints([2, 1, 0, 3]));
    // Lookups still work after the rebuild.
    assert!(matches!(b.get(&Key::Int(2)), Some(gsc::Value::Int(2))));
    a.remove(&Key::Int(1));
    assert_eq!(keys(&a), ints([2, 0]));
}
