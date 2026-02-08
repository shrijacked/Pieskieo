# PostgreSQL Feature: Foreign Keys

**Status**: 🔴 Not Started  
**Priority**: High  
**Dependencies**: 10-unique-constraints.md, 08-deadlock-detection.md  
**Estimated Effort**: 3-4 weeks

---

## Overview

Foreign keys enforce referential integrity between tables by ensuring that a value in one table exists as a primary key in another table. This prevents orphaned records and maintains data consistency across relationships.

**Example**: An `orders` table should only reference `user_id` values that exist in the `users` table.

---

## SQL Syntax

### Creating Foreign Keys

```sql
-- Inline constraint
CREATE TABLE orders (
    id UUID PRIMARY KEY,
    user_id UUID REFERENCES users(id),
    total DECIMAL
);

-- Named constraint
CREATE TABLE orders (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    total DECIMAL,
    CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users(id)
);

-- Composite foreign key
CREATE TABLE order_items (
    order_id UUID,
    item_number INT,
    product_id UUID REFERENCES products(id),
    quantity INT,
    PRIMARY KEY (order_id, item_number),
    FOREIGN KEY (order_id) REFERENCES orders(id)
);
```

### Referential Actions

```sql
-- ON DELETE CASCADE: Delete children when parent deleted
CREATE TABLE comments (
    id UUID PRIMARY KEY,
    post_id UUID REFERENCES posts(id) ON DELETE CASCADE,
    content TEXT
);

-- ON DELETE SET NULL: Set child FK to NULL when parent deleted
CREATE TABLE tasks (
    id UUID PRIMARY KEY,
    assignee_id UUID REFERENCES users(id) ON DELETE SET NULL,
    title TEXT
);

-- ON DELETE SET DEFAULT
CREATE TABLE items (
    id UUID PRIMARY KEY,
    category_id UUID REFERENCES categories(id) ON DELETE SET DEFAULT DEFAULT 'uncategorized'
);

-- ON DELETE RESTRICT: Prevent deletion if children exist (default)
CREATE TABLE customers (
    id UUID PRIMARY KEY,
    name TEXT
);
CREATE TABLE invoices (
    id UUID PRIMARY KEY,
    customer_id UUID REFERENCES customers(id) ON DELETE RESTRICT
);

-- ON UPDATE CASCADE: Update children when parent key changes
CREATE TABLE audit_log (
    id UUID PRIMARY KEY,
    user_id UUID REFERENCES users(id) ON UPDATE CASCADE,
    action TEXT
);
```

### Adding/Dropping Foreign Keys

```sql
-- Add FK to existing table
ALTER TABLE orders ADD CONSTRAINT fk_user 
    FOREIGN KEY (user_id) REFERENCES users(id);

-- Drop FK
ALTER TABLE orders DROP CONSTRAINT fk_user;

-- Disable FK checking temporarily (for bulk operations)
ALTER TABLE orders DISABLE CONSTRAINT fk_user;
ALTER TABLE orders ENABLE CONSTRAINT fk_user;
```

---

## Implementation Plan

### Phase 1: Schema Metadata

**File**: `crates/pieskieo-core/src/schema.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    
    pub on_delete: ReferentialAction,
    pub on_update: ReferentialAction,
    
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReferentialAction {
    NoAction,      // Same as RESTRICT in most cases
    Restrict,      // Prevent delete/update if children exist
    Cascade,       // Delete/update children
    SetNull,       // Set FK columns to NULL
    SetDefault,    // Set FK columns to DEFAULT
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<Column>,
    pub primary_key: Option<PrimaryKey>,
    pub unique_constraints: Vec<UniqueConstraint>,
    
    // NEW
    pub foreign_keys: Vec<ForeignKey>,
}

impl TableSchema {
    pub fn add_foreign_key(&mut self, fk: ForeignKey) -> Result<()> {
        // Validate FK
        self.validate_foreign_key(&fk)?;
        
        // Check for duplicate name
        if self.foreign_keys.iter().any(|f| f.name == fk.name) {
            return Err(PieskieoError::DuplicateConstraint(fk.name));
        }
        
        self.foreign_keys.push(fk);
        Ok(())
    }
    
    fn validate_foreign_key(&self, fk: &ForeignKey) -> Result<()> {
        // 1. Columns must exist
        for col in &fk.columns {
            if !self.columns.iter().any(|c| &c.name == col) {
                return Err(PieskieoError::InvalidColumn(col.clone()));
            }
        }
        
        // 2. Column count must match referenced columns
        if fk.columns.len() != fk.referenced_columns.len() {
            return Err(PieskieoError::InvalidForeignKey(
                "column count mismatch".into()
            ));
        }
        
        // 3. Data types must be compatible
        // (checked at runtime when referenced table is loaded)
        
        Ok(())
    }
}
```

### Phase 2: Foreign Key Validation

**File**: `crates/pieskieo-core/src/foreign_key_validator.rs`

```rust
pub struct ForeignKeyValidator {
    db: Arc<PieskieoDb>,
}

impl ForeignKeyValidator {
    pub fn validate_insert(
        &self,
        table: &str,
        record: &serde_json::Value,
        txn_id: Uuid,
    ) -> Result<()> {
        let schema = self.db.get_schema(table)?;
        
        for fk in &schema.foreign_keys {
            if !fk.enabled {
                continue;
            }
            
            // Extract FK values from record
            let fk_values = self.extract_fk_values(record, &fk.columns)?;
            
            // Skip validation if any FK column is NULL (unless NOT NULL)
            if fk_values.iter().any(|v| v.is_null()) {
                continue;
            }
            
            // Check if referenced record exists
            if !self.referenced_record_exists(
                &fk.referenced_table,
                &fk.referenced_columns,
                &fk_values,
                txn_id,
            )? {
                return Err(PieskieoError::ForeignKeyViolation {
                    constraint: fk.name.clone(),
                    message: format!(
                        "Key ({}) not present in table {}",
                        fk.columns.join(", "),
                        fk.referenced_table
                    ),
                });
            }
        }
        
        Ok(())
    }
    
    pub fn validate_update(
        &self,
        table: &str,
        old_record: &serde_json::Value,
        new_record: &serde_json::Value,
        txn_id: Uuid,
    ) -> Result<()> {
        // Same as insert validation for FK columns
        self.validate_insert(table, new_record, txn_id)?;
        
        // Also check if this table is referenced by others
        self.check_referenced_by(table, old_record, new_record, txn_id)?;
        
        Ok(())
    }
    
    pub fn validate_delete(
        &self,
        table: &str,
        record: &serde_json::Value,
        txn_id: Uuid,
    ) -> Result<()> {
        let all_schemas = self.db.get_all_schemas()?;
        
        // Find all FKs that reference this table
        for schema in all_schemas {
            for fk in &schema.foreign_keys {
                if fk.referenced_table != table || !fk.enabled {
                    continue;
                }
                
                match fk.on_delete {
                    ReferentialAction::Restrict | ReferentialAction::NoAction => {
                        // Check if any child records exist
                        if self.has_referencing_records(&schema.name, fk, record, txn_id)? {
                            return Err(PieskieoError::ForeignKeyViolation {
                                constraint: fk.name.clone(),
                                message: format!(
                                    "Cannot delete: referenced by table {}",
                                    schema.name
                                ),
                            });
                        }
                    }
                    
                    ReferentialAction::Cascade => {
                        // Will be handled in cascade deletion phase
                    }
                    
                    ReferentialAction::SetNull => {
                        // Will be handled in cascade update phase
                    }
                    
                    ReferentialAction::SetDefault => {
                        // Will be handled in cascade update phase
                    }
                }
            }
        }
        
        Ok(())
    }
    
    fn referenced_record_exists(
        &self,
        table: &str,
        columns: &[String],
        values: &[serde_json::Value],
        txn_id: Uuid,
    ) -> Result<bool> {
        // Build WHERE clause: col1 = val1 AND col2 = val2 ...
        let conditions: Vec<String> = columns.iter()
            .zip(values.iter())
            .map(|(col, val)| format!("{} = {}", col, self.value_to_sql(val)))
            .collect();
        
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE {}",
            table,
            conditions.join(" AND ")
        );
        
        let result = self.db.execute_sql_in_transaction(&sql, txn_id)?;
        let count: i64 = result.rows[0]["count"].as_i64().unwrap_or(0);
        
        Ok(count > 0)
    }
    
    fn has_referencing_records(
        &self,
        child_table: &str,
        fk: &ForeignKey,
        parent_record: &serde_json::Value,
        txn_id: Uuid,
    ) -> Result<bool> {
        // Extract parent key values
        let parent_values = self.extract_fk_values(parent_record, &fk.referenced_columns)?;
        
        // Check if any child records reference this parent
        let conditions: Vec<String> = fk.columns.iter()
            .zip(parent_values.iter())
            .map(|(col, val)| format!("{} = {}", col, self.value_to_sql(val)))
            .collect();
        
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE {}",
            child_table,
            conditions.join(" AND ")
        );
        
        let result = self.db.execute_sql_in_transaction(&sql, txn_id)?;
        let count: i64 = result.rows[0]["count"].as_i64().unwrap_or(0);
        
        Ok(count > 0)
    }
}
```

### Phase 3: Cascading Actions

**File**: `crates/pieskieo-core/src/foreign_key_cascade.rs`

```rust
pub struct CascadeExecutor {
    db: Arc<PieskieoDb>,
}

impl CascadeExecutor {
    pub fn cascade_delete(
        &self,
        table: &str,
        record: &serde_json::Value,
        txn_id: Uuid,
    ) -> Result<()> {
        let all_schemas = self.db.get_all_schemas()?;
        
        for schema in all_schemas {
            for fk in &schema.foreign_keys {
                if fk.referenced_table != table || !fk.enabled {
                    continue;
                }
                
                match fk.on_delete {
                    ReferentialAction::Cascade => {
                        // Find and delete child records
                        let children = self.find_referencing_records(
                            &schema.name,
                            fk,
                            record,
                            txn_id,
                        )?;
                        
                        for child in children {
                            // Recursive delete (to handle multi-level cascades)
                            self.cascade_delete(&schema.name, &child, txn_id)?;
                            
                            // Delete the child
                            self.db.delete_record_in_transaction(
                                &schema.name,
                                child["id"].as_str().unwrap(),
                                txn_id,
                            )?;
                        }
                    }
                    
                    ReferentialAction::SetNull => {
                        // Set FK columns to NULL
                        let children = self.find_referencing_records(
                            &schema.name,
                            fk,
                            record,
                            txn_id,
                        )?;
                        
                        for mut child in children {
                            for col in &fk.columns {
                                child[col] = serde_json::Value::Null;
                            }
                            
                            self.db.update_record_in_transaction(
                                &schema.name,
                                child["id"].as_str().unwrap(),
                                &child,
                                txn_id,
                            )?;
                        }
                    }
                    
                    ReferentialAction::SetDefault => {
                        // Set FK columns to their DEFAULT values
                        let children = self.find_referencing_records(
                            &schema.name,
                            fk,
                            record,
                            txn_id,
                        )?;
                        
                        let child_schema = self.db.get_schema(&schema.name)?;
                        
                        for mut child in children {
                            for col_name in &fk.columns {
                                let col = child_schema.columns.iter()
                                    .find(|c| &c.name == col_name)
                                    .unwrap();
                                
                                if let Some(default) = &col.default {
                                    child[col_name] = default.clone();
                                }
                            }
                            
                            self.db.update_record_in_transaction(
                                &schema.name,
                                child["id"].as_str().unwrap(),
                                &child,
                                txn_id,
                            )?;
                        }
                    }
                    
                    _ => {}
                }
            }
        }
        
        Ok(())
    }
    
    pub fn cascade_update(
        &self,
        table: &str,
        old_record: &serde_json::Value,
        new_record: &serde_json::Value,
        txn_id: Uuid,
    ) -> Result<()> {
        // Check if primary key changed
        let schema = self.db.get_schema(table)?;
        let pk_changed = schema.primary_key.as_ref().map_or(false, |pk| {
            pk.columns.iter().any(|col| old_record[col] != new_record[col])
        });
        
        if !pk_changed {
            return Ok(());
        }
        
        // Find all FKs referencing this table
        let all_schemas = self.db.get_all_schemas()?;
        
        for child_schema in all_schemas {
            for fk in &child_schema.foreign_keys {
                if fk.referenced_table != table || !fk.enabled {
                    continue;
                }
                
                if matches!(fk.on_update, ReferentialAction::Cascade) {
                    // Update child records with new key values
                    let children = self.find_referencing_records(
                        &child_schema.name,
                        fk,
                        old_record,
                        txn_id,
                    )?;
                    
                    for mut child in children {
                        // Update FK columns to new values
                        for (fk_col, ref_col) in fk.columns.iter().zip(&fk.referenced_columns) {
                            child[fk_col] = new_record[ref_col].clone();
                        }
                        
                        self.db.update_record_in_transaction(
                            &child_schema.name,
                            child["id"].as_str().unwrap(),
                            &child,
                            txn_id,
                        )?;
                    }
                }
            }
        }
        
        Ok(())
    }
    
    fn find_referencing_records(
        &self,
        child_table: &str,
        fk: &ForeignKey,
        parent_record: &serde_json::Value,
        txn_id: Uuid,
    ) -> Result<Vec<serde_json::Value>> {
        let parent_values: Vec<_> = fk.referenced_columns.iter()
            .map(|col| parent_record[col].clone())
            .collect();
        
        let conditions: Vec<String> = fk.columns.iter()
            .zip(parent_values.iter())
            .map(|(col, val)| format!("{} = {}", col, self.value_to_sql(val)))
            .collect();
        
        let sql = format!(
            "SELECT * FROM {} WHERE {}",
            child_table,
            conditions.join(" AND ")
        );
        
        let result = self.db.execute_sql_in_transaction(&sql, txn_id)?;
        Ok(result.rows)
    }
}
```

### Phase 4: Integration with CRUD Operations

**File**: `crates/pieskieo-core/src/engine.rs`

```rust
impl PieskieoDb {
    pub fn insert(&self, table: &str, record: serde_json::Value) -> Result<Uuid> {
        let txn_id = self.current_transaction_id()?;
        
        // Validate foreign keys BEFORE inserting
        let validator = ForeignKeyValidator::new(Arc::new(self.clone()));
        validator.validate_insert(table, &record, txn_id)?;
        
        // Perform insert
        let record_id = self.insert_internal(table, record)?;
        
        Ok(record_id)
    }
    
    pub fn update(&self, table: &str, id: &str, new_record: serde_json::Value) -> Result<()> {
        let txn_id = self.current_transaction_id()?;
        
        // Get old record
        let old_record = self.get_record(table, id)?;
        
        // Validate foreign keys
        let validator = ForeignKeyValidator::new(Arc::new(self.clone()));
        validator.validate_update(table, &old_record, &new_record, txn_id)?;
        
        // Perform update
        self.update_internal(table, id, new_record.clone())?;
        
        // Handle cascade updates
        let cascade_executor = CascadeExecutor::new(Arc::new(self.clone()));
        cascade_executor.cascade_update(table, &old_record, &new_record, txn_id)?;
        
        Ok(())
    }
    
    pub fn delete(&self, table: &str, id: &str) -> Result<()> {
        let txn_id = self.current_transaction_id()?;
        
        // Get record to delete
        let record = self.get_record(table, id)?;
        
        // Validate deletion (check RESTRICT constraints)
        let validator = ForeignKeyValidator::new(Arc::new(self.clone()));
        validator.validate_delete(table, &record, txn_id)?;
        
        // Handle cascade deletes
        let cascade_executor = CascadeExecutor::new(Arc::new(self.clone()));
        cascade_executor.cascade_delete(table, &record, txn_id)?;
        
        // Perform delete
        self.delete_internal(table, id)?;
        
        Ok(())
    }
}
```

---

## Test Cases

### Test 1: Basic Foreign Key Validation
```sql
CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);
CREATE TABLE posts (
    id UUID PRIMARY KEY,
    user_id UUID REFERENCES users(id),
    title TEXT
);

INSERT INTO users (id, name) VALUES ('u1', 'Alice');
INSERT INTO posts (id, user_id, title) VALUES ('p1', 'u1', 'Hello'); -- OK

-- Should fail: user 'u2' doesn't exist
INSERT INTO posts (id, user_id, title) VALUES ('p2', 'u2', 'World'); 
-- ERROR: foreign key violation
```

### Test 2: ON DELETE CASCADE
```sql
CREATE TABLE authors (id UUID PRIMARY KEY, name TEXT);
CREATE TABLE books (
    id UUID PRIMARY KEY,
    author_id UUID REFERENCES authors(id) ON DELETE CASCADE,
    title TEXT
);

INSERT INTO authors (id, name) VALUES ('a1', 'Tolkien');
INSERT INTO books (id, author_id, title) VALUES ('b1', 'a1', 'LOTR');

DELETE FROM authors WHERE id = 'a1';

SELECT COUNT(*) FROM books WHERE author_id = 'a1'; 
-- Expected: 0 (book was cascade deleted)
```

### Test 3: ON DELETE SET NULL
```sql
CREATE TABLE teams (id UUID PRIMARY KEY, name TEXT);
CREATE TABLE employees (
    id UUID PRIMARY KEY,
    team_id UUID REFERENCES teams(id) ON DELETE SET NULL,
    name TEXT
);

INSERT INTO teams (id, name) VALUES ('t1', 'Engineering');
INSERT INTO employees (id, team_id, name) VALUES ('e1', 't1', 'Bob');

DELETE FROM teams WHERE id = 't1';

SELECT team_id FROM employees WHERE id = 'e1';
-- Expected: NULL
```

### Test 4: ON DELETE RESTRICT
```sql
CREATE TABLE categories (id UUID PRIMARY KEY, name TEXT);
CREATE TABLE products (
    id UUID PRIMARY KEY,
    category_id UUID REFERENCES categories(id) ON DELETE RESTRICT,
    name TEXT
);

INSERT INTO categories (id, name) VALUES ('c1', 'Electronics');
INSERT INTO products (id, category_id, name) VALUES ('p1', 'c1', 'Laptop');

DELETE FROM categories WHERE id = 'c1';
-- ERROR: foreign key violation (product exists)

-- Must delete products first
DELETE FROM products WHERE id = 'p1';
DELETE FROM categories WHERE id = 'c1'; -- Now OK
```

### Test 5: Composite Foreign Key
```sql
CREATE TABLE orders (
    order_id UUID,
    order_version INT,
    total DECIMAL,
    PRIMARY KEY (order_id, order_version)
);

CREATE TABLE line_items (
    id UUID PRIMARY KEY,
    order_id UUID,
    order_version INT,
    product TEXT,
    FOREIGN KEY (order_id, order_version) REFERENCES orders(order_id, order_version)
);

INSERT INTO orders VALUES ('o1', 1, 100);
INSERT INTO line_items VALUES ('li1', 'o1', 1, 'Widget'); -- OK
INSERT INTO line_items VALUES ('li2', 'o1', 2, 'Gadget'); -- ERROR: version 2 doesn't exist
```

### Test 6: Self-Referencing FK
```sql
CREATE TABLE employees (
    id UUID PRIMARY KEY,
    name TEXT,
    manager_id UUID REFERENCES employees(id)
);

INSERT INTO employees (id, name, manager_id) VALUES ('e1', 'CEO', NULL);
INSERT INTO employees (id, name, manager_id) VALUES ('e2', 'VP', 'e1'); -- OK
INSERT INTO employees (id, name, manager_id) VALUES ('e3', 'Manager', 'e2'); -- OK
```

---

## Performance Considerations

### 1. Index on Foreign Key Columns
**Critical**: FK columns should be indexed for fast lookups

```rust
impl TableSchema {
    pub fn add_foreign_key(&mut self, fk: ForeignKey) -> Result<()> {
        self.validate_foreign_key(&fk)?;
        
        // Auto-create index on FK columns if not exists
        if !self.has_index_on_columns(&fk.columns) {
            let index_name = format!("fk_idx_{}_{}", self.name, fk.name);
            self.create_index(Index {
                name: index_name,
                columns: fk.columns.clone(),
                index_type: IndexType::BTree,
                unique: false,
            })?;
        }
        
        self.foreign_keys.push(fk);
        Ok(())
    }
}
```

### 2. Deferred Constraint Checking
For better performance in bulk operations:

```sql
-- Disable FK checks temporarily
SET CONSTRAINTS ALL DEFERRED;

-- Bulk insert
INSERT INTO orders SELECT * FROM staging_orders;

-- FK checked at COMMIT time
COMMIT;
```

### 3. Cascade Optimization
Batch cascade operations instead of one-by-one:

```rust
impl CascadeExecutor {
    pub fn cascade_delete_batch(
        &self,
        table: &str,
        records: &[serde_json::Value],
        txn_id: Uuid,
    ) -> Result<()> {
        // Group by FK to execute in batches
        let all_schemas = self.db.get_all_schemas()?;
        
        for schema in all_schemas {
            for fk in &schema.foreign_keys {
                if fk.on_delete == ReferentialAction::Cascade {
                    // Build batch DELETE
                    let ids: Vec<_> = records.iter()
                        .flat_map(|r| self.find_referencing_record_ids(&schema.name, fk, r, txn_id))
                        .collect();
                    
                    if !ids.is_empty() {
                        let sql = format!(
                            "DELETE FROM {} WHERE id IN ({})",
                            schema.name,
                            ids.iter().map(|id| format!("'{}'", id)).join(", ")
                        );
                        self.db.execute_sql_in_transaction(&sql, txn_id)?;
                    }
                }
            }
        }
        
        Ok(())
    }
}
```

---

## Metrics to Track

- `pieskieo_foreign_key_validations_total` - Counter
- `pieskieo_foreign_key_violations_total` - Counter
- `pieskieo_cascade_deletes_total` - Counter
- `pieskieo_cascade_updates_total` - Counter
- `pieskieo_foreign_key_validation_duration_ms` - Histogram

---

## Implementation Checklist

- [ ] Add ForeignKey struct to schema
- [ ] Implement FK parsing in CREATE TABLE
- [ ] Implement FK parsing in ALTER TABLE
- [ ] Create ForeignKeyValidator
- [ ] Add validate_insert logic
- [ ] Add validate_update logic
- [ ] Add validate_delete logic
- [ ] Create CascadeExecutor
- [ ] Implement ON DELETE CASCADE
- [ ] Implement ON DELETE SET NULL
- [ ] Implement ON DELETE SET DEFAULT
- [ ] Implement ON DELETE RESTRICT
- [ ] Implement ON UPDATE CASCADE
- [ ] Auto-create indexes on FK columns
- [ ] Add comprehensive FK tests
- [ ] Test multi-level cascades
- [ ] Test composite FKs
- [ ] Test self-referencing FKs
- [ ] Test circular FK references
- [ ] Add FK violation error messages
- [ ] Document FK behavior
- [ ] Add metrics and monitoring
- [ ] Performance benchmark cascade operations

---

## Advanced Foreign Key Features

### Deferred Constraint Checking

```rust
#[derive(Debug, Clone)]
pub enum ConstraintTiming {
    Immediate,  // Check after each statement
    Deferred,   // Check at COMMIT time
}

#[derive(Debug, Clone)]
pub struct ForeignKey {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_delete: ReferentialAction,
    pub on_update: ReferentialAction,
    pub enabled: bool,
    
    // NEW: Deferrable constraints
    pub deferrable: bool,
    pub initially: ConstraintTiming,
}

impl PieskieoDb {
    pub fn set_constraints(&self, mode: ConstraintMode) -> Result<()> {
        // SQL: SET CONSTRAINTS ALL DEFERRED;
        //      SET CONSTRAINTS fk_name IMMEDIATE;
        
        let mut txn = self.current_transaction_mut()?;
        
        match mode {
            ConstraintMode::AllDeferred => {
                txn.deferred_constraints = DeferredConstraints::All;
            }
            ConstraintMode::AllImmediate => {
                // Check all deferred constraints NOW
                self.check_deferred_constraints(&txn)?;
                txn.deferred_constraints = DeferredConstraints::None;
            }
            ConstraintMode::Specific { names, deferred } => {
                for name in names {
                    if deferred {
                        txn.deferred_constraint_set.insert(name);
                    } else {
                        // Check this constraint now
                        self.check_specific_constraint(&txn, &name)?;
                        txn.deferred_constraint_set.remove(&name);
                    }
                }
            }
        }
        
        Ok(())
    }
    
    fn check_deferred_constraints_at_commit(&self, txn: &Transaction) -> Result<()> {
        // Called during COMMIT - check all deferred FKs
        
        for (table, record_id) in &txn.inserted_records {
            let record = self.get_record(table, record_id)?;
            let schema = self.get_schema(table)?;
            
            for fk in &schema.foreign_keys {
                if !self.should_check_deferred(txn, &fk.name) {
                    continue;
                }
                
                // Validate FK at commit time
                let validator = ForeignKeyValidator::new(Arc::new(self.clone()));
                validator.validate_insert(table, &record, txn.id)?;
            }
        }
        
        // Also check updates and deletes
        self.check_deferred_updates(txn)?;
        self.check_deferred_deletes(txn)?;
        
        Ok(())
    }
}

/// Example usage:
/// ```sql
/// BEGIN;
/// 
/// SET CONSTRAINTS ALL DEFERRED;
/// 
/// -- These will NOT be checked immediately
/// INSERT INTO orders (id, customer_id) VALUES ('o1', 'c999'); -- FK violation OK for now
/// INSERT INTO customers (id, name) VALUES ('c999', 'New Customer'); -- Satisfies FK
/// 
/// COMMIT; -- Now FK check passes
/// ```
```

### MATCH PARTIAL and MATCH FULL

```rust
#[derive(Debug, Clone)]
pub enum MatchType {
    Simple,    // Default: any NULL in FK columns → skip check
    Full,      // All FK columns NULL or all non-NULL
    Partial,   // NOT FULLY SUPPORTED in PostgreSQL, we'll implement it
}

impl ForeignKeyValidator {
    fn validate_match_type(
        &self,
        fk: &ForeignKey,
        fk_values: &[Value],
    ) -> Result<bool> {
        let null_count = fk_values.iter().filter(|v| v.is_null()).count();
        
        match fk.match_type {
            MatchType::Simple => {
                // If ANY column is NULL, skip validation
                Ok(null_count > 0)
            }
            
            MatchType::Full => {
                // Either ALL NULL or NONE NULL
                if null_count > 0 && null_count < fk_values.len() {
                    return Err(PieskieoError::ForeignKeyViolation {
                        constraint: fk.name.clone(),
                        message: "MATCH FULL: partial NULL not allowed".into(),
                    });
                }
                Ok(null_count == fk_values.len()) // Skip if all NULL
            }
            
            MatchType::Partial => {
                // Complex: check non-NULL columns only
                // Build WHERE clause with only non-NULL columns
                let non_null_cols: Vec<_> = fk_values.iter()
                    .enumerate()
                    .filter(|(_, v)| !v.is_null())
                    .collect();
                
                if non_null_cols.is_empty() {
                    return Ok(true); // All NULL, skip
                }
                
                // Check if referenced record exists matching non-NULL columns
                let conditions: Vec<String> = non_null_cols.iter()
                    .map(|(i, v)| {
                        format!("{} = {}", fk.referenced_columns[*i], self.value_to_sql(v))
                    })
                    .collect();
                
                let sql = format!(
                    "SELECT 1 FROM {} WHERE {} LIMIT 1",
                    fk.referenced_table,
                    conditions.join(" AND ")
                );
                
                let result = self.db.execute_sql(&sql)?;
                Ok(result.rows.is_empty()) // Skip if not found (will error later)
            }
        }
    }
}
```

### Cross-Shard Foreign Keys

```rust
pub struct DistributedForeignKeyValidator {
    coordinator: Arc<Coordinator>,
}

impl DistributedForeignKeyValidator {
    pub async fn validate_cross_shard_fk(
        &self,
        fk: &ForeignKey,
        fk_values: &[Value],
        txn_id: Uuid,
    ) -> Result<()> {
        // Determine which shard holds the referenced table
        let referenced_shard = self.coordinator
            .get_shard_for_table(&fk.referenced_table)
            .await?;
        
        let current_shard = self.coordinator.current_shard_id();
        
        if referenced_shard == current_shard {
            // Same shard - use local validation
            return self.validate_local_fk(fk, fk_values, txn_id);
        }
        
        // Cross-shard FK - need distributed validation
        let request = ValidateFKRequest {
            table: fk.referenced_table.clone(),
            columns: fk.referenced_columns.clone(),
            values: fk_values.to_vec(),
            txn_id,
        };
        
        // Send validation request to remote shard
        let response = self.coordinator
            .send_to_shard(referenced_shard, request)
            .await?;
        
        if !response.exists {
            return Err(PieskieoError::ForeignKeyViolation {
                constraint: fk.name.clone(),
                message: format!(
                    "Key ({}) not present in table {} (shard {})",
                    fk.columns.join(", "),
                    fk.referenced_table,
                    referenced_shard
                ),
            });
        }
        
        // Record cross-shard dependency for 2PC
        self.coordinator.register_cross_shard_dependency(
            txn_id,
            current_shard,
            referenced_shard,
        ).await?;
        
        Ok(())
    }
    
    pub async fn cascade_delete_cross_shard(
        &self,
        table: &str,
        record: &Value,
        txn_id: Uuid,
    ) -> Result<()> {
        // Find all child tables (may be on different shards)
        let child_tables = self.find_child_tables(table)?;
        
        for (child_table, fk) in child_tables {
            let child_shard = self.coordinator
                .get_shard_for_table(&child_table)
                .await?;
            
            if fk.on_delete != ReferentialAction::Cascade {
                continue;
            }
            
            // Find child records (potentially remote)
            let children = if child_shard == self.coordinator.current_shard_id() {
                self.find_local_children(&child_table, &fk, record)?
            } else {
                self.find_remote_children(child_shard, &child_table, &fk, record).await?
            };
            
            // Recursively cascade delete children
            for child in children {
                if child_shard == self.coordinator.current_shard_id() {
                    self.cascade_delete_cross_shard(&child_table, &child, txn_id).await?;
                } else {
                    // Initiate cascade delete on remote shard
                    self.coordinator.remote_cascade_delete(
                        child_shard,
                        &child_table,
                        &child,
                        txn_id,
                    ).await?;
                }
            }
        }
        
        Ok(())
    }
}
```

### Cycle Detection for Cascades

```rust
pub struct CascadeDepthTracker {
    // Prevent infinite recursion in cascade operations
    visited: HashSet<(String, Uuid)>, // (table, record_id)
    max_depth: usize,
    current_depth: usize,
}

impl CascadeExecutor {
    pub fn cascade_delete_with_cycle_detection(
        &mut self,
        table: &str,
        record: &Value,
        txn_id: Uuid,
        tracker: &mut CascadeDepthTracker,
    ) -> Result<()> {
        tracker.current_depth += 1;
        
        if tracker.current_depth > tracker.max_depth {
            return Err(PieskieoError::CascadeTooDeep {
                max_depth: tracker.max_depth,
                current_depth: tracker.current_depth,
            });
        }
        
        let record_id = record.get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PieskieoError::Internal("no id in record".into()))?;
        
        let key = (table.to_string(), Uuid::parse_str(record_id)?);
        
        if tracker.visited.contains(&key) {
            // Cycle detected - skip to avoid infinite loop
            warn!("Cycle detected in cascade delete: {:?}", key);
            tracker.current_depth -= 1;
            return Ok(());
        }
        
        tracker.visited.insert(key);
        
        // Perform cascade delete
        self.cascade_delete_internal(table, record, txn_id, tracker)?;
        
        tracker.current_depth -= 1;
        Ok(())
    }
}
```

## Production Operations

### Monitoring & Alerting

```rust
// FK validation metrics
metrics::counter!("pieskieo_fk_validations_total",
                  "type" => "insert|update|delete",
                  "cross_shard" => is_cross_shard).increment(1);
metrics::counter!("pieskieo_fk_violations_total",
                  "constraint" => fk_name).increment(1);
metrics::histogram!("pieskieo_fk_validation_duration_ms").record(duration);

// Cascade operation metrics
metrics::counter!("pieskieo_cascade_operations_total",
                  "action" => "delete|update|set_null").increment(1);
metrics::histogram!("pieskieo_cascade_depth").record(depth);
metrics::histogram!("pieskieo_cascade_records_affected").record(count);
metrics::histogram!("pieskieo_cascade_duration_ms").record(duration);

// Deferred constraint metrics
metrics::gauge!("pieskieo_deferred_constraints_pending").set(pending_count);
metrics::counter!("pieskieo_deferred_constraint_violations_at_commit_total").increment(1);
```

### Configuration

```toml
[foreign_keys]
# Enable cross-shard foreign keys
cross_shard_enabled = true

# Maximum cascade depth (prevent infinite loops)
max_cascade_depth = 50

# Batch size for cascade operations
cascade_batch_size = 1000

# Deferred constraints enabled by default
deferred_by_default = false

# MATCH type for new FKs
default_match_type = "simple"  # or "full", "partial"

# Timeout for cross-shard FK validation (ms)
cross_shard_timeout_ms = 5000
```

---

**Created**: 2026-02-08  
**Author**: Implementation Team  
**Review Status**: Production-Ready
