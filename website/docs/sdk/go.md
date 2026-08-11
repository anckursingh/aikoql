---
title: Go SDK
description: TCP JSON-RPC client for Go
---

# Go SDK

Standard library only — zero external dependencies.

```go
import "github.com/ancku/aikoql-sdk"

client := aikoql.NewClient("127.0.0.1:9090")
client.Connect()

// Create
result, _ := client.Remember(map[string]interface{}{
    "type_name": "Employee",
    "properties": map[string]interface{}{"name": "Alice"},
})
fmt.Println(result.Koid)
```
