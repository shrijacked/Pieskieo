# PostgreSQL Feature: JSON/JSONB Operators

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: 16-gin-indexes.md (GIN indexes for JSONB)  
**Estimated Effort**: 3-4 weeks

---

## Overview

PostgreSQL's JSON/JSONB support allows storing, querying, and manipulating JSON documents within relational tables. JSONB is the binary format (faster, indexable) vs JSON (text format, preserves formatting).

**Key Difference**:
- `JSON`: Stores exact text, preserves whitespace/key order, slower
- `JSONB`: Binary format, faster queries, supports indexing, decomposes keys

For Pieskieo, we'll **only implement JSONB** (like modern Postgres recommends).

---

## SQL Syntax & Operators

### 1. Basic Storage & Retrieval
```sql
CREATE TABLE users (
    id UUID PRIMARY KEY,
    profile JSONB
);

INSERT INTO users VALUES (
    'u1',
    '{"name": "Alice", "age": 30, "tags": ["developer", "golang"]}'::JSONB
);

-- Retrieve entire JSON
SELECT profile FROM users WHERE id = 'u1';
```

### 2. Extraction Operators

```sql
-- -> : Get JSON object field (returns JSONB)
SELECT profile -> 'name' FROM users;
-- Result: "Alice" (JSONB)

-- ->> : Get JSON object field as text
SELECT profile ->> 'name' FROM users;
-- Result: Alice (TEXT)

-- #> : Get nested path (array of keys)
SELECT profile #> '{address, city}' FROM users;

-- #>> : Get nested path as text
SELECT profile #>> '{address, city}' FROM users;

-- Array indexing
SELECT profile -> 'tags' -> 0 FROM users;
-- Result: "developer"
```

### 3. Containment Operators

```sql
-- @> : Does left JSONB contain right JSONB?
SELECT * FROM users WHERE profile @> '{"age": 30}';

-- <@ : Is left JSONB contained in right?
SELECT * FROM users WHERE '{"age": 30}' <@ profile;

-- ? : Does JSON contain key?
SELECT * FROM users WHERE profile ? 'email';

-- ?| : Does JSON contain any of these keys?
SELECT * FROM users WHERE profile ?| array['email', 'phone'];

-- ?& : Does JSON contain all of these keys?
SELECT * FROM users WHERE profile ?& array['name', 'age'];
```

### 4. Modification Functions

```sql
-- jsonb_set: Update a value at path
UPDATE users SET profile = jsonb_set(
    profile,
    '{address, city}',
    '"San Francisco"'
) WHERE id = 'u1';

-- || : Concatenate / merge JSON
UPDATE users SET profile = profile || '{"verified": true}';

-- - : Delete key
UPDATE users SET profile = profile - 'temporary_field';

-- #- : Delete at path
UPDATE users SET profile = profile #- '{settings, notifications}';

-- jsonb_insert: Insert value at path
UPDATE users SET profile = jsonb_insert(
    profile,
    '{tags, 0}',
    '"admin"'
);
```

### 5. Query Functions

```sql
-- jsonb_array_elements: Expand array to rows
SELECT jsonb_array_elements(profile -> 'tags') AS tag FROM users;

-- jsonb_each: Expand object to key-value rows
SELECT * FROM jsonb_each((SELECT profile FROM users WHERE id = 'u1'));

-- jsonb_object_keys: Get all keys
SELECT jsonb_object_keys(profile) FROM users;

-- jsonb_array_length: Get array length
SELECT jsonb_array_length(profile -> 'tags') FROM users;

-- jsonb_typeof: Get type of JSON value
SELECT jsonb_typeof(profile -> 'age'); -- "number"
```

---

## Implementation Plan

### Phase 1: JSONB Storage Format

**File**: `crates/pieskieo-core/src/jsonb.rs`

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSONB is stored as binary format for fast access
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Jsonb {
    // Internal binary representation
    // Format: similar to PostgreSQL's JSONB
    // [version: u8][flags: u8][data: bytes]
    data: Vec<u8>,
}

impl Jsonb {
    pub fn from_str(json: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(json)?;
        Self::from_value(value)
    }
    
    pub fn from_value(value: Value) -> Result<Self> {
        // Encode JSON value to binary format
        let data = Self::encode(&value)?;
        Ok(Self { data })
    }
    
    pub fn to_value(&self) -> Result<Value> {
        // Decode binary to JSON value
        Self::decode(&self.data)
    }
    
    fn encode(value: &Value) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        
        // Version (1 byte)
        buf.push(1);
        
        // Encode value recursively
        Self::encode_value(value, &mut buf)?;
        
        Ok(buf)
    }
    
    fn encode_value(value: &Value, buf: &mut Vec<u8>) -> Result<()> {
        match value {
            Value::Null => {
                buf.push(0x00); // Null tag
            }
            Value::Bool(b) => {
                buf.push(0x01); // Bool tag
                buf.push(if *b { 1 } else { 0 });
            }
            Value::Number(n) => {
                buf.push(0x02); // Number tag
                if let Some(i) = n.as_i64() {
                    buf.push(0x01); // Integer subtype
                    buf.extend_from_slice(&i.to_le_bytes());
                } else if let Some(f) = n.as_f64() {
                    buf.push(0x02); // Float subtype
                    buf.extend_from_slice(&f.to_le_bytes());
                }
            }
            Value::String(s) => {
                buf.push(0x03); // String tag
                let bytes = s.as_bytes();
                buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(bytes);
            }
            Value::Array(arr) => {
                buf.push(0x04); // Array tag
                buf.extend_from_slice(&(arr.len() as u32).to_le_bytes());
                for item in arr {
                    Self::encode_value(item, buf)?;
                }
            }
            Value::Object(obj) => {
                buf.push(0x05); // Object tag
                buf.extend_from_slice(&(obj.len() as u32).to_le_bytes());
                
                // Sort keys for consistent encoding (JSONB property)
                let mut keys: Vec<_> = obj.keys().collect();
                keys.sort();
                
                for key in keys {
                    // Encode key
                    let key_bytes = key.as_bytes();
                    buf.extend_from_slice(&(key_bytes.len() as u32).to_le_bytes());
                    buf.extend_from_slice(key_bytes);
                    
                    // Encode value
                    Self::encode_value(&obj[key], buf)?;
                }
            }
        }
        
        Ok(())
    }
    
    fn decode(data: &[u8]) -> Result<Value> {
        let mut cursor = std::io::Cursor::new(data);
        
        // Read version
        let version = Self::read_u8(&mut cursor)?;
        if version != 1 {
            return Err(PieskieoError::InvalidJsonb("unsupported version".into()));
        }
        
        Self::decode_value(&mut cursor)
    }
    
    fn decode_value(cursor: &mut std::io::Cursor<&[u8]>) -> Result<Value> {
        let tag = Self::read_u8(cursor)?;
        
        match tag {
            0x00 => Ok(Value::Null),
            0x01 => {
                let b = Self::read_u8(cursor)?;
                Ok(Value::Bool(b != 0))
            }
            0x02 => {
                let subtype = Self::read_u8(cursor)?;
                match subtype {
                    0x01 => {
                        let i = Self::read_i64(cursor)?;
                        Ok(Value::Number(i.into()))
                    }
                    0x02 => {
                        let f = Self::read_f64(cursor)?;
                        Ok(serde_json::json!(f))
                    }
                    _ => Err(PieskieoError::InvalidJsonb("unknown number subtype".into())),
                }
            }
            0x03 => {
                let len = Self::read_u32(cursor)? as usize;
                let s = Self::read_string(cursor, len)?;
                Ok(Value::String(s))
            }
            0x04 => {
                let len = Self::read_u32(cursor)? as usize;
                let mut arr = Vec::with_capacity(len);
                for _ in 0..len {
                    arr.push(Self::decode_value(cursor)?);
                }
                Ok(Value::Array(arr))
            }
            0x05 => {
                let len = Self::read_u32(cursor)? as usize;
                let mut obj = serde_json::Map::new();
                for _ in 0..len {
                    let key_len = Self::read_u32(cursor)? as usize;
                    let key = Self::read_string(cursor, key_len)?;
                    let value = Self::decode_value(cursor)?;
                    obj.insert(key, value);
                }
                Ok(Value::Object(obj))
            }
            _ => Err(PieskieoError::InvalidJsonb(format!("unknown tag: {}", tag))),
        }
    }
}
```

### Phase 2: JSONB Operators

**File**: `crates/pieskieo-core/src/jsonb_ops.rs`

```rust
impl Jsonb {
    /// -> operator: Extract field
    pub fn get(&self, key: &str) -> Result<Option<Jsonb>> {
        let value = self.to_value()?;
        
        if let Value::Object(obj) = value {
            if let Some(field) = obj.get(key) {
                return Ok(Some(Jsonb::from_value(field.clone())?));
            }
        }
        
        Ok(None)
    }
    
    /// ->> operator: Extract field as text
    pub fn get_text(&self, key: &str) -> Result<Option<String>> {
        if let Some(jsonb) = self.get(key)? {
            let value = jsonb.to_value()?;
            match value {
                Value::String(s) => Ok(Some(s)),
                Value::Number(n) => Ok(Some(n.to_string())),
                Value::Bool(b) => Ok(Some(b.to_string())),
                Value::Null => Ok(None),
                _ => Ok(Some(value.to_string())),
            }
        } else {
            Ok(None)
        }
    }
    
    /// #> operator: Extract at path
    pub fn get_path(&self, path: &[&str]) -> Result<Option<Jsonb>> {
        let mut current = self.to_value()?;
        
        for key in path {
            match current {
                Value::Object(obj) => {
                    if let Some(next) = obj.get(*key) {
                        current = next.clone();
                    } else {
                        return Ok(None);
                    }
                }
                Value::Array(arr) => {
                    if let Ok(index) = key.parse::<usize>() {
                        if let Some(next) = arr.get(index) {
                            current = next.clone();
                        } else {
                            return Ok(None);
                        }
                    } else {
                        return Ok(None);
                    }
                }
                _ => return Ok(None),
            }
        }
        
        Ok(Some(Jsonb::from_value(current)?))
    }
    
    /// @> operator: Does self contain other?
    pub fn contains(&self, other: &Jsonb) -> Result<bool> {
        let self_value = self.to_value()?;
        let other_value = other.to_value()?;
        
        Ok(Self::contains_value(&self_value, &other_value))
    }
    
    fn contains_value(container: &Value, contained: &Value) -> bool {
        match (container, contained) {
            (Value::Object(c_obj), Value::Object(o_obj)) => {
                // All keys in 'contained' must exist in 'container' with same values
                for (key, value) in o_obj {
                    if let Some(c_value) = c_obj.get(key) {
                        if !Self::contains_value(c_value, value) {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                true
            }
            (Value::Array(c_arr), Value::Array(o_arr)) => {
                // All elements in 'contained' must be in 'container'
                for o_item in o_arr {
                    if !c_arr.iter().any(|c_item| Self::contains_value(c_item, o_item)) {
                        return false;
                    }
                }
                true
            }
            (a, b) => a == b,
        }
    }
    
    /// ? operator: Does JSON contain key?
    pub fn has_key(&self, key: &str) -> Result<bool> {
        let value = self.to_value()?;
        if let Value::Object(obj) = value {
            Ok(obj.contains_key(key))
        } else {
            Ok(false)
        }
    }
    
    /// || operator: Merge/concatenate
    pub fn merge(&self, other: &Jsonb) -> Result<Jsonb> {
        let mut self_value = self.to_value()?;
        let other_value = other.to_value()?;
        
        if let (Value::Object(ref mut self_obj), Value::Object(other_obj)) = (&mut self_value, other_value) {
            for (key, value) in other_obj {
                self_obj.insert(key, value);
            }
        }
        
        Jsonb::from_value(self_value)
    }
    
    /// - operator: Delete key
    pub fn delete_key(&self, key: &str) -> Result<Jsonb> {
        let mut value = self.to_value()?;
        
        if let Value::Object(ref mut obj) = value {
            obj.remove(key);
        }
        
        Jsonb::from_value(value)
    }
    
    /// jsonb_set: Update value at path
    pub fn set_path(&self, path: &[&str], new_value: Jsonb) -> Result<Jsonb> {
        let mut root = self.to_value()?;
        let new_val = new_value.to_value()?;
        
        Self::set_path_recursive(&mut root, path, new_val)?;
        
        Jsonb::from_value(root)
    }
    
    fn set_path_recursive(current: &mut Value, path: &[&str], new_value: Value) -> Result<()> {
        if path.is_empty() {
            *current = new_value;
            return Ok(());
        }
        
        let key = path[0];
        let remaining_path = &path[1..];
        
        match current {
            Value::Object(obj) => {
                if remaining_path.is_empty() {
                    obj.insert(key.to_string(), new_value);
                } else {
                    if !obj.contains_key(key) {
                        obj.insert(key.to_string(), Value::Object(serde_json::Map::new()));
                    }
                    Self::set_path_recursive(obj.get_mut(key).unwrap(), remaining_path, new_value)?;
                }
            }
            _ => {
                return Err(PieskieoError::InvalidJsonb("cannot set path on non-object".into()));
            }
        }
        
        Ok(())
    }
}
```

### Phase 3: GIN Index for JSONB

**File**: `crates/pieskieo-core/src/index/gin_jsonb.rs`

```rust
/// GIN index for JSONB containment queries (@>, ?, etc.)
pub struct JsonbGinIndex {
    pub name: String,
    pub table: String,
    pub column: String,
    
    // Map: JSON path -> list of record IDs
    pub path_index: HashMap<JsonPath, Vec<Uuid>>,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct JsonPath {
    pub keys: Vec<String>,
    pub value: Option<serde_json::Value>,
}

impl JsonbGinIndex {
    pub fn index_jsonb(&mut self, record_id: Uuid, jsonb: &Jsonb) -> Result<()> {
        let value = jsonb.to_value()?;
        
        // Extract all paths from JSON
        let paths = Self::extract_paths(&value, Vec::new());
        
        for path in paths {
            self.path_index.entry(path).or_insert_with(Vec::new).push(record_id);
        }
        
        Ok(())
    }
    
    fn extract_paths(value: &Value, current_path: Vec<String>) -> Vec<JsonPath> {
        let mut paths = Vec::new();
        
        match value {
            Value::Object(obj) => {
                for (key, val) in obj {
                    let mut path = current_path.clone();
                    path.push(key.clone());
                    
                    // Add this key path
                    paths.push(JsonPath {
                        keys: path.clone(),
                        value: Some(val.clone()),
                    });
                    
                    // Recurse
                    paths.extend(Self::extract_paths(val, path));
                }
            }
            Value::Array(arr) => {
                for (i, val) in arr.iter().enumerate() {
                    let mut path = current_path.clone();
                    path.push(i.to_string());
                    paths.extend(Self::extract_paths(val, path));
                }
            }
            _ => {}
        }
        
        paths
    }
    
    pub fn search_contains(&self, query_jsonb: &Jsonb) -> Result<Vec<Uuid>> {
        // Find all records where JSONB contains query
        let query_value = query_jsonb.to_value()?;
        let query_paths = Self::extract_paths(&query_value, Vec::new());
        
        // Get intersection of all matching record IDs
        let mut result: Option<HashSet<Uuid>> = None;
        
        for path in query_paths {
            if let Some(ids) = self.path_index.get(&path) {
                let id_set: HashSet<_> = ids.iter().copied().collect();
                
                result = Some(match result {
                    Some(existing) => existing.intersection(&id_set).copied().collect(),
                    None => id_set,
                });
            } else {
                // Path not found, no results
                return Ok(Vec::new());
            }
        }
        
        Ok(result.unwrap_or_default().into_iter().collect())
    }
}
```

### Phase 4: SQL Integration

**File**: `crates/pieskieo-core/src/engine.rs`

```rust
impl PieskieoDb {
    pub fn execute_jsonb_query(&self, query: &SelectStatement) -> Result<SqlResult> {
        // Parse WHERE clause for JSONB operators
        // profile @> '{"age": 30}'
        // profile ? 'email'
        // profile -> 'name' = 'Alice'
        
        if let Some(jsonb_condition) = self.extract_jsonb_condition(&query.where_clause)? {
            // Check for GIN index
            if let Some(index) = self.find_gin_index(&query.table, &jsonb_condition.column)? {
                // Use GIN index
                let record_ids = index.search_contains(&jsonb_condition.value)?;
                
                let mut results = Vec::new();
                for id in record_ids {
                    if let Some(record) = self.get_record_by_id(id)? {
                        results.push(record);
                    }
                }
                
                return Ok(SqlResult { rows: results });
            }
        }
        
        // Fall back to sequential scan
        self.execute_sequential_scan(query)
    }
}
```

---

## Test Cases

### Test 1: Basic JSONB Storage
```sql
CREATE TABLE products (
    id UUID PRIMARY KEY,
    data JSONB
);

INSERT INTO products VALUES (
    'p1',
    '{"name": "Laptop", "price": 999, "specs": {"ram": "16GB", "cpu": "Intel i7"}}'
);

SELECT data -> 'name' FROM products WHERE id = 'p1';
-- Expected: "Laptop"

SELECT data ->> 'name' FROM products WHERE id = 'p1';
-- Expected: Laptop (text)
```

### Test 2: Nested Path Extraction
```sql
SELECT data #> '{specs, ram}' FROM products WHERE id = 'p1';
-- Expected: "16GB"

SELECT data #>> '{specs, ram}' FROM products WHERE id = 'p1';
-- Expected: 16GB (text)
```

### Test 3: Containment Query
```sql
CREATE INDEX idx_product_data ON products USING GIN (data);

SELECT * FROM products WHERE data @> '{"price": 999}';
-- Should use GIN index, return p1
```

### Test 4: Key Existence
```sql
SELECT * FROM products WHERE data ? 'warranty';
-- Returns products with "warranty" field
```

### Test 5: Array Operations
```sql
INSERT INTO products VALUES (
    'p2',
    '{"name": "Phone", "tags": ["electronics", "mobile", "5G"]}'
);

SELECT jsonb_array_elements_text(data -> 'tags') AS tag 
FROM products WHERE id = 'p2';
-- Expected: 3 rows (electronics, mobile, 5G)
```

### Test 6: Modification
```sql
UPDATE products SET data = jsonb_set(
    data,
    '{specs, ssd}',
    '"512GB"'
) WHERE id = 'p1';

SELECT data FROM products WHERE id = 'p1';
-- Should have new ssd field
```

---

## Performance Considerations

### 1. GIN Index Tuning
```rust
const GIN_PENDING_LIST_LIMIT: usize = 4 * 1024 * 1024; // 4MB

impl JsonbGinIndex {
    pub fn with_fastupdate(mut self, enabled: bool) -> Self {
        self.fast_update = enabled;
        self
    }
}
```

### 2. JSONB Compression
Large JSON objects should be compressed:
```rust
impl Jsonb {
    pub fn encode_compressed(value: &Value) -> Result<Vec<u8>> {
        let raw = Self::encode(value)?;
        if raw.len() > 1024 {
            // Use zstd compression
            zstd::encode_all(&raw[..], 3)
        } else {
            Ok(raw)
        }
    }
}
```

### 3. Partial Indexes
```sql
-- Index only products with warranty
CREATE INDEX idx_warranty ON products USING GIN (data) 
WHERE data ? 'warranty';
```

---

## Metrics to Track

- `pieskieo_jsonb_operations_total{op="get|set|contains"}` - Counter
- `pieskieo_jsonb_index_searches_total` - Counter
- `pieskieo_jsonb_size_bytes` - Histogram

---

## Implementation Checklist

- [ ] Implement JSONB binary encoding/decoding
- [ ] Add JSONB type to schema
- [ ] Implement -> operator (get field)
- [ ] Implement ->> operator (get field as text)
- [ ] Implement #> operator (get path)
- [ ] Implement #>> operator (get path as text)
- [ ] Implement @> operator (containment)
- [ ] Implement <@ operator (contained by)
- [ ] Implement ? operator (key exists)
- [ ] Implement ?| operator (any key exists)
- [ ] Implement ?& operator (all keys exist)
- [ ] Implement || operator (merge)
- [ ] Implement - operator (delete key)
- [ ] Implement jsonb_set function
- [ ] Implement jsonb_insert function
- [ ] Implement jsonb_array_elements function
- [ ] Implement jsonb_each function
- [ ] Create GIN index for JSONB
- [ ] Add parser support for JSONB operators
- [ ] Test all operators
- [ ] Test GIN index performance
- [ ] Add JSONB compression
- [ ] Document JSONB usage

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status: Production-Ready
