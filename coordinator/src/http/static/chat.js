import { getHostname, getReadyLlmModel } from '/static/models.js';

let thread = null;
let input = null;
let sendBtn = null;
let busy = false;
const MAX_CONTEXT_TURNS = 20; // 10 exchanges; oldest pair dropped when exceeded

let conversationContext = [];
let lastModelKey = null;

export function init(panel) {
  panel.innerHTML = `
    <div class="chat-thread" id="chat-thread"></div>
    <div class="chat-input-bar">
      <textarea class="chat-input" id="chat-input" rows="2"
        placeholder="Ask anything or control your home…"></textarea>
      <div class="chat-btn-row">
        <span class="chat-ctx-counter" id="chat-ctx-counter"></span>
        <button class="chat-send" id="chat-send">Send</button>
        <button class="chat-new" id="chat-new">New</button>
        <button class="chat-clear" id="chat-clear">Clear</button>
      </div>
    </div>`;

  thread  = panel.querySelector('#chat-thread');
  input   = panel.querySelector('#chat-input');
  sendBtn = panel.querySelector('#chat-send');
  const clearBtn = panel.querySelector('#chat-clear');
  const newBtn   = panel.querySelector('#chat-new');

  requestAnimationFrame(() => { if (thread) thread.scrollTop = thread.scrollHeight; });

  sendBtn.addEventListener('click', () => send());
  clearBtn.addEventListener('click', () => clear());
  newBtn.addEventListener('click', () => newContext());
  input.addEventListener('keydown', e => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  });
}

async function send() {
  const text = input.value.trim();
  if (!text || busy) return;

  const currentModelKey = getReadyLlmModel();
  if (currentModelKey && lastModelKey && currentModelKey !== lastModelKey) {
    newContext('model changed — new conversation');
  }

  input.value = '';
  busy = true;
  sendBtn.disabled = true;

  appendMsg('user', text);

  const thinking = appendThinking();

  const token = localStorage.getItem('meshToken') ?? '';
  try {
    const t0 = Date.now();
    const res = await fetch(`/api/chat?token=${encodeURIComponent(token)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text, context: conversationContext }),
    });
    const totalMs = Date.now() - t0;

    thinking.remove();

    if (!res.ok) {
      appendMsg('assistant', `Error: ${res.status} ${res.statusText}`);
    } else {
      const data = await res.json();
      if (data.error) {
        appendMsg('assistant', `Error: ${data.error}`);
      } else if (data.tool_calls && data.tool_calls.length > 0) {
        for (const tc of data.tool_calls) {
          const result = tc.result ?? '';
          appendToolMsg(tc.tool, result, data.node_id, data.model_name, data.duration_ms, data.tokens_generated, totalMs, data.prompt_eval_ms ?? 0);
        }
        if (data.text) {
          appendMsg('assistant', data.text, data.node_id, data.model_name, data.duration_ms, data.tokens_generated, totalMs, data.prompt_eval_ms ?? 0);
        }
        const assistantContent = data.text ||
          data.tool_calls.map(tc => `${tc.tool}: ${tc.result ?? ''}`).join('; ');
        conversationContext.push({ role: 'User', content: text });
        conversationContext.push({ role: 'Assistant', content: assistantContent });
        trimAndUpdateContext();
        if (data.node_id && data.model_name) lastModelKey = `${data.node_id}/${data.model_name}`;
      } else {
        const reply = data.text ?? '';
        appendMsg('assistant', reply, data.node_id, data.model_name, data.duration_ms, data.tokens_generated, totalMs, data.prompt_eval_ms ?? 0);
        conversationContext.push({ role: 'User', content: text });
        conversationContext.push({ role: 'Assistant', content: reply });
        trimAndUpdateContext();
        if (data.node_id && data.model_name) lastModelKey = `${data.node_id}/${data.model_name}`;
      }
    }
  } catch (err) {
    thinking.remove();
    appendMsg('assistant', `Network error: ${err.message}`);
  }

  busy = false;
  sendBtn.disabled = false;
  // Blur on mobile so the keyboard dismisses and the response is readable;
  // on desktop the input stays focused for quick follow-ups.
  if (window.matchMedia('(pointer: coarse)').matches) {
    input.blur();
  } else {
    input.focus();
  }
}

function clear() {
  if (thread) thread.innerHTML = '';
  conversationContext = [];
  updateTurnCounter();
}

function newContext(label = 'new conversation') {
  conversationContext = [];
  updateTurnCounter();
  if (!thread) return;
  const divider = document.createElement('div');
  divider.className = 'chat-divider';
  divider.textContent = label;
  thread.appendChild(divider);
  thread.scrollTop = thread.scrollHeight;
}

function trimAndUpdateContext() {
  if (conversationContext.length > MAX_CONTEXT_TURNS) {
    conversationContext = conversationContext.slice(-MAX_CONTEXT_TURNS);
  }
  updateTurnCounter();
}

function updateTurnCounter() {
  const el = document.getElementById('chat-ctx-counter');
  if (!el) return;
  const n = conversationContext.length;
  el.textContent = n > 0 ? `${n} / ${MAX_CONTEXT_TURNS} turns` : '';
  el.classList.toggle('chat-ctx-near-limit', n >= MAX_CONTEXT_TURNS * 0.8);
}

function appendMsg(role, text, nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs = 0) {
  const div = document.createElement('div');
  div.className = `chat-msg chat-${role}`;
  const bubble = document.createElement('div');
  bubble.className = 'chat-bubble';
  bubble.textContent = text;
  div.appendChild(bubble);
  if (nodeId && role === 'assistant') {
    const meta = document.createElement('div');
    meta.className = 'chat-meta';
    meta.textContent = metaLine(nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs);
    div.appendChild(meta);
  }
  thread.appendChild(div);
  thread.scrollTop = thread.scrollHeight;
  return div;
}

function appendToolMsg(tool, result, nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs = 0) {
  const div = document.createElement('div');
  div.className = 'chat-msg chat-assistant';
  const bubble = document.createElement('div');
  bubble.className = 'chat-bubble chat-tool';
  bubble.textContent = `🔧 ${tool} → ${result}`;
  div.appendChild(bubble);
  if (nodeId) {
    const meta = document.createElement('div');
    meta.className = 'chat-meta';
    meta.textContent = metaLine(nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs);
    div.appendChild(meta);
  }
  thread.appendChild(div);
  thread.scrollTop = thread.scrollHeight;
  return div;
}

function metaLine(nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs = 0) {
  const host = getHostname(nodeId);
  let line = modelName ? `${host} · ${modelName}` : host;
  if (totalMs > 0) {
    const total  = (totalMs / 1000).toFixed(1);
    const prefill = prefillMs > 0 ? `${(prefillMs / 1000).toFixed(1)}s prefill · ` : '';
    const gen    = durationMs > 0 ? `${(durationMs / 1000).toFixed(1)}s gen` : '—';
    const tps    = tokensGenerated > 0 && durationMs > 0 ? ` · ${(tokensGenerated / (durationMs / 1000)).toFixed(1)} tok/s` : '';
    line += ` · ${total}s (${prefill}${gen})${tps}`;
  }
  return line;
}

function appendThinking() {
  const div = document.createElement('div');
  div.className = 'chat-msg chat-assistant';
  const bubble = document.createElement('div');
  bubble.className = 'chat-bubble chat-thinking';
  bubble.textContent = '…';
  div.appendChild(bubble);
  thread.appendChild(div);
  thread.scrollTop = thread.scrollHeight;
  return div;
}
