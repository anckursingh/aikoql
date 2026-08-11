---
title: Java SDK
description: Zero-dependency JSON-RPC client for Java
---

# Java SDK

Zero dependencies — compiles with plain `javac`.

```java
AikoqlClient client = new AikoqlClient("127.0.0.1", 9090);
client.connect();

String result = client.remember(
    "{\"type_name\": \"Employee\", \"properties\": {\"name\": \"Alice\"}}"
);
System.out.println(result);
```
