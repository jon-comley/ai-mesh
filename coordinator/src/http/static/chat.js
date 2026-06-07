import { getHostname } from '/static/models.js';

let thread = null;
let input = null;
let sendBtn = null;
let busy = false;

export function init(panel) {
  panel.innerHTML = `
    <div class="chat-thread" id="chat-thread"></div>
    <div class="chat-input-bar">
      <textarea class="chat-input" id="chat-input" rows="2"
        placeholder="Ask anything or control your home…"></textarea>
      <div class="chat-btn-row">
        <button class="chat-send" id="chat-send">Send</button>
        <button class="chat-clear" id="chat-clear">Clear</button>
      </div>
    </div>`;

  thread  = panel.querySelector('#chat-thread');
  input   = panel.querySelector('#chat-input');
  sendBtn = panel.querySelector('#chat-send');
  const clearBtn = panel.querySelector('#chat-clear');

  requestAnimationFrame(() => { if (thread) thread.scrollTop = thread.scrollHeight; });

  sendBtn.addEventListener('click', () => send());
  clearBtn.addEventListener('click', () => clear());
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

  input.value = '';
  busy = true;
  sendBtn.disabled = true;

  appendMsg('user', text);

  const thinking = appendThinking();

  const token = localStorage.getItem('meshToken') ?? '';
  try {
    const res = await fetch(`/api/chat?token=${encodeURIComponent(token)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text, context: [] }),
    });

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
          appendToolMsg(tc.tool, result, data.node_id, data.model_name);
        }
        if (data.text) {
          appendMsg('assistant', data.text, data.node_id, data.model_name);
        }
      } else {
        const reply = data.text ?? '';
        appendMsg('assistant', reply, data.node_id, data.model_name);
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
}

function appendMsg(role, text, nodeId, modelName) {
  const div = document.createElement('div');
  div.className = `chat-msg chat-${role}`;
  const bubble = document.createElement('div');
  bubble.className = 'chat-bubble';
  bubble.textContent = text;
  div.appendChild(bubble);
  if (nodeId && role === 'assistant') {
    const meta = document.createElement('div');
    meta.className = 'chat-meta';
    meta.textContent = metaLine(nodeId, modelName);
    div.appendChild(meta);
  }
  thread.appendChild(div);
  thread.scrollTop = thread.scrollHeight;
  return div;
}

function appendToolMsg(tool, result, nodeId, modelName) {
  const div = document.createElement('div');
  div.className = 'chat-msg chat-assistant';
  const bubble = document.createElement('div');
  bubble.className = 'chat-bubble chat-tool';
  bubble.textContent = `🔧 ${tool} → ${result}`;
  div.appendChild(bubble);
  if (nodeId) {
    const meta = document.createElement('div');
    meta.className = 'chat-meta';
    meta.textContent = metaLine(nodeId, modelName);
    div.appendChild(meta);
  }
  thread.appendChild(div);
  thread.scrollTop = thread.scrollHeight;
  return div;
}

function metaLine(nodeId, modelName) {
  const host = getHostname(nodeId);
  return modelName ? `${host} · ${modelName}` : host;
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
