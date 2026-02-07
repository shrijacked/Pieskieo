# Pieskieo Python SDK

Simple sync client for Pieskieo HTTP API.

## Install (editable)
```
pip install -e .
```

## Usage
```python
from pieskieo import PieskieoClient

c = PieskieoClient("http://localhost:8000")

# Namespaced vector insert + search
vec_id = c.put_vector([0.1,0.2,0.3], meta={"type":"demo"}, namespace="team-a")
hits = c.search([0.1,0.2,0.3], k=3, namespace="team-a")
print(hits)

# Collection-aware docs (Mongo-like)
doc_id = c.put_doc({"user": "alice"}, namespace="team-a", collection="users")
doc = c.get_doc(doc_id, namespace="team-a", collection="users")

# Table-aware rows (Postgres-like)
row_id = c.put_row({"item": "widget", "price": 9.99}, namespace="team-a", table="orders")
rows = c.query_rows({"item": "widget"}, namespace="team-a", table="orders")

# Pagination
rows_page_2 = c.query_rows({"item": "widget"}, limit=50, offset=50, namespace="team-a", table="orders")

# SQL-like queries
docs = c.query_docs(sql="SELECT * FROM team-a.users WHERE user = 'alice' LIMIT 10")

# Async client
import asyncio
from pieskieo import AsyncPieskieoClient, models

async def main():
    async with AsyncPieskieoClient("http://localhost:8000") as ac:
        req = models.VectorInput(vector=[0.1,0.2,0.3], namespace="team-a")
        vid = await ac.put_vector(req.vector, namespace=req.namespace)
        hits = await ac.search([0.1, 0.2, 0.3], k=3, namespace="team-a")
        print(hits)

asyncio.run(main())
```
