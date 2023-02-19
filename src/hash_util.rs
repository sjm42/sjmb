// hash_util.rs

use std::collections::HashMap;

pub fn get_wild<'a, T>(map: &'a HashMap<String, T>, key: &str) -> Option<&'a T> {
    map.get(key).or_else(|| map.get("*"))
}

// EOF
