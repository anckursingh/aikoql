// Aikoql Go SDK — MCP JSON-RPC client over TCP.
//
// Usage:
//   db := NewClient("127.0.0.1:9090")
//   db.Initialize()
//   ko, _ := db.Remember(RememberParams{TypeName: "fact", Properties: map[string]any{"x": 1}})

package aikoql

import (
	"bufio"
	"encoding/json"
	"fmt"
	"net"
)

// ---- Types ----------------------------------------------------------------

type RememberParams struct {
	Subject    string                 `json:"subject,omitempty"`
	TypeName   string                 `json:"type_name"`
	KOID       string                 `json:"koid,omitempty"`
	Properties map[string]interface{} `json:"properties,omitempty"`
	Note       string                 `json:"note,omitempty"`
}

type Remembered struct {
	KOID     string `json:"koid"`
	Version  uint64 `json:"version"`
	CommitTS uint64 `json:"commit_ts"`
}

type KnowledgeObject struct {
	KOID       string                 `json:"koid"`
	Version    uint64                 `json:"version"`
	Properties map[string]interface{} `json:"properties"`
}

type ScoredKO struct {
	KOID     string  `json:"koid"`
	Score    float64 `json:"score"`
	TypeName string  `json:"type_name"`
}

type Metrics struct {
	JournalSeq    uint64            `json:"journal_seq"`
	TotalObjects  int               `json:"total_objects"`
	ActiveObjects int               `json:"active_objects"`
	UptimeSeconds float64           `json:"uptime_seconds"`
	ByLifecycle   map[string]int    `json:"by_lifecycle"`
	ByType        map[string]int    `json:"by_type"`
}

// ---- Client ---------------------------------------------------------------

type Client struct {
	conn   net.Conn
	reader *bufio.Reader
	nextID int
}

func NewClient(addr string) (*Client, error) {
	conn, err := net.Dial("tcp", addr)
	if err != nil {
		return nil, err
	}
	return &Client{conn: conn, reader: bufio.NewReader(conn)}, nil
}

func (c *Client) Close() error {
	return c.conn.Close()
}

func (c *Client) Initialize() error {
	_, err := c.request("initialize", map[string]interface{}{
		"protocolVersion": "2024-11-05",
		"capabilities":    map[string]interface{}{},
		"clientInfo": map[string]interface{}{
			"name": "aikoql-go-sdk", "version": "0.1.0",
		},
	})
	return err
}

// ---- MCP transport --------------------------------------------------------

type rpcRequest struct {
	JSONRPC string      `json:"jsonrpc"`
	ID      int         `json:"id"`
	Method  string      `json:"method"`
	Params  interface{} `json:"params"`
}

type rpcResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      int             `json:"id"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *rpcError       `json:"error,omitempty"`
}

type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

func (c *Client) request(method string, params interface{}) (json.RawMessage, error) {
	c.nextID++
	req := rpcRequest{JSONRPC: "2.0", ID: c.nextID, Method: method, Params: params}
	data, _ := json.Marshal(req)
	fmt.Fprintf(c.conn, "%s\n", data)

	for {
		line, err := c.reader.ReadString('\n')
		if err != nil {
			return nil, err
		}
		var resp rpcResponse
		if err := json.Unmarshal([]byte(line), &resp); err != nil {
			continue
		}
		if resp.ID == c.nextID {
			if resp.Error != nil {
				return nil, fmt.Errorf("rpc error %d: %s", resp.Error.Code, resp.Error.Message)
			}
			return resp.Result, nil
		}
	}
}

type toolCallResult struct {
	Content []struct {
		Text string `json:"text"`
	} `json:"content"`
	IsError bool `json:"isError"`
}

func (c *Client) callTool(name string, args map[string]interface{}) (json.RawMessage, error) {
	raw, err := c.request("tools/call", map[string]interface{}{
		"name": name, "arguments": args,
	})
	if err != nil {
		return nil, err
	}
	var res toolCallResult
	if err := json.Unmarshal(raw, &res); err != nil {
		return nil, err
	}
	if res.IsError {
		return nil, fmt.Errorf("tool error: %s", res.Content[0].Text)
	}
	return json.RawMessage(res.Content[0].Text), nil
}

// ---- Knowledge Object tools -----------------------------------------------

func (c *Client) Remember(params RememberParams) (*Remembered, error) {
	if params.Subject == "" {
		params.Subject = "sdk-user"
	}
	args := map[string]interface{}{}
	data, _ := json.Marshal(params)
	json.Unmarshal(data, &args)
	raw, err := c.callTool("remember", args)
	if err != nil {
		return nil, err
	}
	var r Remembered
	json.Unmarshal(raw, &r)
	return &r, nil
}

func (c *Client) Get(koid string) (*KnowledgeObject, error) {
	raw, err := c.callTool("get", map[string]interface{}{
		"subject": "sdk-user", "koid": koid,
	})
	if err != nil {
		return nil, err
	}
	var ko KnowledgeObject
	json.Unmarshal(raw, &ko)
	return &ko, nil
}

func (c *Client) FindSimilar(text string, k int) ([]ScoredKO, error) {
	raw, err := c.callTool("find_similar", map[string]interface{}{
		"subject": "sdk-user", "text": text, "k": k,
	})
	if err != nil {
		return nil, err
	}
	var result struct {
		Results []ScoredKO `json:"results"`
	}
	json.Unmarshal(raw, &result)
	return result.Results, nil
}

func (c *Client) Aikoql(query string) (json.RawMessage, error) {
	return c.callTool("aikoql", map[string]interface{}{
		"subject": "sdk-user", "query": query,
	})
}

func (c *Client) Backup() (map[string]interface{}, error) {
	raw, err := c.callTool("backup", map[string]interface{}{})
	if err != nil {
		return nil, err
	}
	var result map[string]interface{}
	json.Unmarshal(raw, &result)
	return result, nil
}

func (c *Client) Metrics() (*Metrics, error) {
	raw, err := c.callTool("metrics", map[string]interface{}{})
	if err != nil {
		return nil, err
	}
	var m Metrics
	json.Unmarshal(raw, &m)
	return &m, nil
}
