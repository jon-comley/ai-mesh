import { getHostname } from '/static/models.js';
import { setPref } from '/static/prefs.js';

const MAX_CONTEXT_TURNS = 20; // 10 exchanges; oldest pair dropped when exceeded
const VOICE_PREF_KEY = 'voice-in-chat'; // '0' = hidden; anything else = shown (default on)

// Mounts a chat thread + input bar into `container`, wired to POST /api/chat.
// Each call is an independent conversation (its own context/state and class-scoped
// element lookups), so the same widget runs on both the Chat tab and the Reaper tab
// without clashing IDs or sharing history. Returns a small control handle.
export function createChatWidget(container, { placeholder = 'Ask anything or control your home…', voiceToggle = false } = {}) {
  const voiceToggleHtml = voiceToggle
    ? `<label class="chat-voice-toggle" title="Show voice-assistant exchanges in this chat"><input type="checkbox" class="chat-voice-cb"> 🎤 voice</label>`
    : '';
  container.innerHTML = `
    <div class="chat-thread"></div>
    <div class="chat-input-bar">
      <textarea class="chat-input" rows="2" placeholder="${placeholder}"></textarea>
      <div class="chat-btn-row">
        ${voiceToggleHtml}
        <span class="chat-ctx-counter"></span>
        <button class="chat-send">Send</button>
        <button class="chat-new">New</button>
        <button class="chat-clear">Clear</button>
      </div>
    </div>`;

  const thread     = container.querySelector('.chat-thread');
  const input      = container.querySelector('.chat-input');
  const sendBtn    = container.querySelector('.chat-send');
  const clearBtn   = container.querySelector('.chat-clear');
  const newBtn     = container.querySelector('.chat-new');
  const ctxCounter = container.querySelector('.chat-ctx-counter');
  const voiceCb    = container.querySelector('.chat-voice-cb');

  if (voiceCb) {
    voiceCb.checked = localStorage.getItem(VOICE_PREF_KEY) !== '0';
    voiceCb.addEventListener('change', () => {
      setPref(VOICE_PREF_KEY, voiceCb.checked ? '1' : '0');
    });
  }

  let busy = false;
  let conversationContext = [];
  let lastModelKey = null;

  requestAnimationFrame(() => { thread.scrollTop = thread.scrollHeight; });

  sendBtn.addEventListener('click', () => send());
  clearBtn.addEventListener('click', () => clear());
  newBtn.addEventListener('click', () => newContext());
  input.addEventListener('keydown', e => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  });

  async function send() {
    const text = input.value.trim();
    if (!text || busy) return;

    input.value = '';
    busy = true;
    sendBtn.disabled = true;

    const userDiv = appendMsg('user', text);

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
        } else {
          // Detect a model switch from the model that ACTUALLY served this response.
          // Server-side routing picks the node/model and the request carries no
          // model_name, so the dashboard cannot reliably predict it pre-send — the old
          // pre-send guess (first Ready model in the map) mismatched the served model
          // whenever >1 model was Ready, wiping context on every turn. Only reset when
          // the served model genuinely changes between turns.
          const servedKey = (data.node_id && data.model_name)
            ? `${data.node_id}/${data.model_name}` : null;
          if (servedKey && lastModelKey && servedKey !== lastModelKey) {
            conversationContext = [];
            updateTurnCounter();
            const divider = document.createElement('div');
            divider.className = 'chat-divider';
            divider.textContent = 'model changed — new conversation';
            thread.insertBefore(divider, userDiv);
          }
          // Render the reply (tool rows and/or a text bubble), then record one
          // User/Assistant turn — the context push + trim is shared by both shapes.
          let assistantContent;
          if (data.tool_calls && data.tool_calls.length > 0) {
            for (const tc of data.tool_calls) {
              appendToolMsg(tc.tool, tc.result ?? '', data.node_id, data.model_name, data.duration_ms, data.tokens_generated, totalMs, data.prompt_eval_ms ?? 0, data.total_ms ?? 0);
            }
            if (data.text) {
              appendMsg('assistant', data.text, data.node_id, data.model_name, data.duration_ms, data.tokens_generated, totalMs, data.prompt_eval_ms ?? 0, data.total_ms ?? 0);
            }
            assistantContent = data.text ||
              data.tool_calls.map(tc => `${tc.tool}: ${tc.result ?? ''}`).join('; ');
          } else {
            assistantContent = data.text ?? '';
            // Placeholder rather than a blank bubble when the model returns no text and
            // no tool calls; conversationContext still records the actual (empty) content.
            appendMsg('assistant', assistantContent || '(no response)', data.node_id, data.model_name, data.duration_ms, data.tokens_generated, totalMs, data.prompt_eval_ms ?? 0, data.total_ms ?? 0);
          }
          conversationContext.push({ role: 'User', content: text });
          conversationContext.push({ role: 'Assistant', content: assistantContent });
          trimAndUpdateContext();
          if (data.node_id && data.model_name) lastModelKey = servedKey;
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
    thread.innerHTML = '';
    conversationContext = [];
    updateTurnCounter();
  }

  function newContext(label = 'new conversation') {
    conversationContext = [];
    updateTurnCounter();
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
    if (!ctxCounter) return;
    const n = conversationContext.length;
    ctxCounter.textContent = n > 0 ? `${n} / ${MAX_CONTEXT_TURNS} turns` : '';
    ctxCounter.classList.toggle('chat-ctx-near-limit', n >= MAX_CONTEXT_TURNS * 0.8);
  }

  function appendMsg(role, text, nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs = 0, serverMs = 0) {
    const div = document.createElement('div');
    div.className = `chat-msg chat-${role}`;
    const bubble = document.createElement('div');
    bubble.className = 'chat-bubble';
    bubble.textContent = text;
    div.appendChild(bubble);
    if (nodeId && role === 'assistant') {
      const meta = document.createElement('div');
      meta.className = 'chat-meta';
      meta.textContent = metaLine(nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs, serverMs);
      div.appendChild(meta);
    }
    thread.appendChild(div);
    thread.scrollTop = thread.scrollHeight;
    return div;
  }

  function appendToolMsg(tool, result, nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs = 0, serverMs = 0) {
    const div = document.createElement('div');
    div.className = 'chat-msg chat-assistant';
    const bubble = document.createElement('div');
    bubble.className = 'chat-bubble chat-tool';
    bubble.textContent = `🔧 ${tool} → ${result}`;
    div.appendChild(bubble);
    if (nodeId) {
      const meta = document.createElement('div');
      meta.className = 'chat-meta';
      meta.textContent = metaLine(nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs, serverMs);
      div.appendChild(meta);
    }
    thread.appendChild(div);
    thread.scrollTop = thread.scrollHeight;
    return div;
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

  // Render one voice-assistant exchange pushed over the WS: 🎤-marked
  // user-side transcript, then the response the same way a typed reply
  // renders (tool rows + text bubble + meta line). Display-only by design —
  // voice turns do NOT join conversationContext, so typed follow-ups
  // can't reference them (deliberate v1 choice; revisit with TTS).
  function appendExchange(evt) {
    if (localStorage.getItem(VOICE_PREF_KEY) === '0') return;
    const userDiv = appendMsg('user', `🎤 ${evt.transcript}`);
    userDiv.classList.add('chat-voice');
    if (evt.error) {
      appendMsg('assistant', `Error: ${evt.error}`).classList.add('chat-voice');
      return;
    }
    for (const tc of evt.tool_calls ?? []) {
      appendToolMsg(tc.tool, tc.result, evt.node_id, evt.model_name, 0, 0, evt.total_ms ?? 0)
        .classList.add('chat-voice');
    }
    if (evt.response || !(evt.tool_calls ?? []).length) {
      appendMsg('assistant', evt.response || '(no response)', evt.node_id, evt.model_name, 0, 0, evt.total_ms ?? 0)
        .classList.add('chat-voice');
    }
  }

  return { thread, clear, newContext, appendExchange };
}

// The Chat tab's live widget instance — the WS voice-exchange handler needs
// to reach it (the Reaper tab's widget deliberately doesn't show voice).
let chatTab = null;

// Chat tab entry point (called by dashboard.js).
export function init(panel) {
  chatTab = createChatWidget(panel, { voiceToggle: true });
}

// WS `VoiceExchange` events land here (see dashboard.js handlers map).
export function handleVoiceExchange(evt) {
  chatTab?.appendExchange(evt);
}

function metaLine(nodeId, modelName, durationMs, tokensGenerated, totalMs, prefillMs = 0, serverMs = 0) {
  const host = getHostname(nodeId);
  let line = modelName ? `${host} · ${modelName}` : host;
  if (totalMs > 0) {
    const total  = (totalMs / 1000).toFixed(1);
    const server = serverMs > 0 ? `${(serverMs / 1000).toFixed(1)}s server · ` : '';
    const prefill = prefillMs > 0 ? `${(prefillMs / 1000).toFixed(1)}s prefill · ` : '';
    const gen    = durationMs > 0 ? `${(durationMs / 1000).toFixed(1)}s gen` : '—';
    const tps    = tokensGenerated > 0 && durationMs > 0 ? ` · ${(tokensGenerated / (durationMs / 1000)).toFixed(1)} tok/s` : '';
    line += ` · ${total}s (${server}${prefill}${gen})${tps}`;
  }
  return line;
}
