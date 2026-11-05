// A fake OpenAI-compatible model for the container test. It answers every
// chat completion with one fixed text, so the test needs no provider key.
// Each request is logged as one JSON line on stderr: the test reads the tool
// names to check which tools each agent gets.
import { createServer } from "node:http";

export const ANSWER = process.env.MOCK_MODEL_ANSWER ?? "fake OpenClaw result";
const port = Number(process.env.MOCK_MODEL_PORT ?? 4010);
const host = process.env.MOCK_MODEL_HOST ?? "127.0.0.1";

function toolNames(body) {
  return (body.tools ?? []).map((tool) => tool.function?.name ?? tool.name).filter(Boolean);
}

function lastUserText(body) {
  const messages = body.messages ?? body.input ?? [];
  const user = [...messages].reverse().find((message) => message.role === "user");
  const content = user?.content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map((part) => part.text ?? "").join("");
  return "";
}

function allText(body) {
  return JSON.stringify(body.messages ?? body.input ?? []);
}

function chunk(id, delta, finish) {
  return `data: ${JSON.stringify({
    id,
    object: "chat.completion.chunk",
    created: Math.floor(Date.now() / 1000),
    model: "mock-model",
    choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`;
}

const server = createServer((request, response) => {
  let raw = "";
  request.on("data", (data) => (raw += data));
  request.on("end", () => {
    if (request.method === "GET" && request.url.endsWith("/models")) {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ object: "list", data: [{ id: "mock-model", object: "model" }] }));
      return;
    }
    if (request.method !== "POST" || !request.url.endsWith("/chat/completions")) {
      process.stderr.write(`${JSON.stringify({ unhandled: `${request.method} ${request.url}` })}\n`);
      response.writeHead(404).end();
      return;
    }
    const body = raw ? JSON.parse(raw) : {};
    // OpenClaw sends its runtime context and the message as separate parts,
    // so search all messages for the framed task and the probe text.
    const text = allText(body);
    process.stderr.write(
      `${JSON.stringify({
        model: body.model,
        tools: toolNames(body),
        aetherTask: text.includes("[Aether task"),
        mainProbe: text.includes("e2e main probe"),
        user: lastUserText(body).slice(-200),
      })}\n`,
    );
    const id = `chatcmpl-${Date.now()}`;
    if (body.stream) {
      response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      response.write(chunk(id, { role: "assistant", content: ANSWER }, null));
      response.write(chunk(id, {}, "stop"));
      if (body.stream_options?.include_usage) {
        response.write(
          `data: ${JSON.stringify({ id, object: "chat.completion.chunk", choices: [], usage: { prompt_tokens: 10, completion_tokens: 3, total_tokens: 13 } })}\n\n`,
        );
      }
      response.end("data: [DONE]\n\n");
      return;
    }
    response.writeHead(200, { "content-type": "application/json" });
    response.end(
      JSON.stringify({
        id,
        object: "chat.completion",
        created: Math.floor(Date.now() / 1000),
        model: "mock-model",
        choices: [{ index: 0, message: { role: "assistant", content: ANSWER }, finish_reason: "stop" }],
        usage: { prompt_tokens: 10, completion_tokens: 3, total_tokens: 13 },
      }),
    );
  });
});

server.listen(port, host, () => process.stderr.write(`${JSON.stringify({ listening: port })}\n`));
