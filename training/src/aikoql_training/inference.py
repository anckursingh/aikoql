"""Model I/O helpers (the design §40 seam): the prompt/parse contract
between the fine-tuned model and the pipeline.

Two skills, two prompts: the query step maps a question to an AikoQL
query alone (QUERY: marker); the answer step maps the question plus
retrieved context statements to a short answer, or the UNKNOWN:
refusal when the context does not contain the answer. parse_model_reply
extracts the markers when present and falls back to an answer-only
reply (a bare refusal); the scorecard re-runs both outputs through
the deterministic checks.
"""

from __future__ import annotations

from typing import List, Tuple

QUERY_MARKER = "QUERY:"
ANSWER_MARKER = "ANSWER:"


def build_query_prompt(question: str) -> str:
    return (
        f"Question: {question}\n"
        "Reply with the AikoQL query that answers the question, "
        "in this exact format:\n"
        "QUERY: <aikoql query>\n"
    )


def build_answer_prompt(question: str, statements: List[str]) -> str:
    lines = "\n".join(f"- {s}" for s in statements)
    return (
        f"Question: {question}\n\n"
        f"Knowledge context:\n{lines}\n\n"
        "Answer the question from the context above. If the context "
        "does not contain the answer, reply exactly: "
        "UNKNOWN: <short reason>. Keep the answer short.\n"
    )


def parse_model_reply(text: str) -> Tuple[str, str]:
    """Split a model reply into (query, answer) on the markers.

    ponytail: plain marker search — a marker string inside a query
    literal would mis-split; the corpus grammar values never carry
    the markers, so the ceiling is theoretical.
    """
    text = (text or "").strip()
    q_i = text.find(QUERY_MARKER)
    a_i = text.find(ANSWER_MARKER)
    if q_i == -1:
        return "", text
    rest = text[q_i + len(QUERY_MARKER):]
    if a_i > q_i:
        return (rest[:a_i - q_i - len(QUERY_MARKER)].strip(),
                text[a_i + len(ANSWER_MARKER):].strip())
    return rest.strip(), ""
