# aikoql-client — Java SDK

The first-party Java client for the [aikoql](https://github.com/anckursingh/aikoql) knowledge database: the canonical Database API (remember, get, query, traverse, hybrid search, context compile, prepare/execute) over the MCP transport. Zero runtime dependencies, hand-rolled JSON, JDK 17+.

```java
import io.aikoql.client.AikoqlClient;
import io.aikoql.client.Connection;

Connection conn = AikoqlClient.dial("127.0.0.1:9090", "s3cret");
ResultSet rows = conn.query("MATCH person RETURN *");
```

```xml
<dependency>
  <groupId>io.aikoql</groupId>
  <artifactId>aikoql-client</artifactId>
  <version>0.2.0</version>
</dependency>
```

- Tests: `mvn test` (real-server conformance honors `AIKOQL_MCP_BIN`)
- Fuzz smokes (7 Jazzer targets): `bash jazzer-smoke.sh` (`AIKOQL_JAZZER_SECONDS` for a bigger budget)
- License: Apache-2.0
