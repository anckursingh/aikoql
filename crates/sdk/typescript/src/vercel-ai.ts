// AG-60: the Vercel AI SDK adapter — aikoql memory as ai v7 tools.
// generateText({ tools: createAikoqlTools(client) }) dispatches
// execute(input, { toolCallId, messages }) on tool calls; each tool maps
// to the canonical Database API (crates/sdk/typescript/src/tools.ts).

import { jsonSchema, tool } from "ai";
import type { ToolSet } from "ai";

import type { Client } from "./client.ts";

export function createAikoqlTools(client: Client, subject = "vercel-ai"): ToolSet {
  return {
    remember: tool({
      description:
        "Store a fact or note in aikoql long-term memory. Returns the KOID of the stored object.",
      inputSchema: jsonSchema<{ type_name: string; properties?: Record<string, unknown> }>({
        type: "object",
        properties: {
          type_name: { type: "string", description: "The object type, e.g. 'note'." },
          properties: { type: "object", description: "The fact's properties." },
        },
        required: ["type_name"],
      }),
      execute: async (input, _options) =>
        client.remember({
          type_name: input.type_name,
          properties: input.properties,
          subject,
        }),
    }),
    recall: tool({
      description:
        "Recall the most relevant stored facts for a query (text search).",
      inputSchema: jsonSchema<{ query: string; k?: number }>({
        type: "object",
        properties: {
          query: { type: "string", description: "What to recall." },
          k: { type: "number", description: "How many results (default 5)." },
        },
        required: ["query"],
      }),
      execute: async (input, _options) =>
        client.findSimilar({
          text: input.query,
          k: input.k ?? 5,
          fusion: "text", // the MCP spelling; "text_only" is the embedded client's
          subject,
          wait_for_freshness_ms: 2000,
        }),
    }),
    forget: tool({
      description: "Tombstone a stored fact by KOID so it stops being recalled.",
      inputSchema: jsonSchema<{ koid: string }>({
        type: "object",
        properties: {
          koid: { type: "string", description: "The KOID remember returned." },
        },
        required: ["koid"],
      }),
      execute: async (input, _options) => client.forget(input.koid, "tombstone", subject),
    }),
  };
}
