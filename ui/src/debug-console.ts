/**
 * Debug Console component — wires up UI controls for relay connection,
 * subscription monitoring, event inspection, and the raw event log.
 */

import * as relay from './relay';
import type { Event } from 'nostr-tools';

// TODO: import WASM bindings when build pipeline is set up
// import { parse_memory_event } from '../pkg/snow_ui';

const SNOW_TAG_PREFIXES = ['snow:tier', 'snow:model', 'snow:confidence', 'snow:source', 'snow:version', 'snow:supersedes'];

/** Highlight snow: tags in a tag array display. */
function renderTags(tags: string[][]): string {
  return tags
    .map((tag) => {
      const key = tag[0] ?? '';
      const isSnow = SNOW_TAG_PREFIXES.some((p) => key.startsWith(p)) || key === 'd';
      const cls = isSnow ? ' class="snow-tag"' : '';
      const escaped = tag.map((v) => escapeHtml(v)).join(', ');
      return `<span${cls}>[${escaped}]</span>`;
    })
    .join('\n');
}

function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function formatEvent(event: Event): string {
  const tags = event.tags as string[][];
  return `<div>
<strong>id:</strong> ${escapeHtml(event.id)}
<strong>kind:</strong> ${event.kind}
<strong>pubkey:</strong> ${escapeHtml(event.pubkey)}
<strong>created_at:</strong> ${event.created_at} (${new Date(event.created_at * 1000).toISOString()})
<strong>tags:</strong>
${renderTags(tags)}
<strong>content:</strong>
${escapeHtml(event.content)}
</div>`;
}

export function init(): void {
  const $ = (id: string) => document.getElementById(id)!;

  const relayUrlInput = $('relay-url') as HTMLInputElement;
  const btnConnect = $('btn-connect') as HTMLButtonElement;
  const btnDisconnect = $('btn-disconnect') as HTMLButtonElement;
  const relayInfo = $('relay-info');
  const statusDot = document.querySelector('.status-dot')!;
  const statusText = $('relay-status-text');

  const btnSubMemory = $('btn-sub-memory') as HTMLButtonElement;
  const subList = $('sub-list');
  const eventCountEl = $('event-count');

  const eventJsonInput = $('event-json') as HTMLTextAreaElement;
  const btnInspect = $('btn-inspect') as HTMLButtonElement;
  const inspectOutput = $('inspect-output');

  const btnClearLog = $('btn-clear-log') as HTMLButtonElement;
  const logEntries = $('log-entries');

  // Relay status updates
  relay.onStatusChange((status) => {
    statusDot.className = `status-dot ${status === 'connected' ? 'online' : status === 'connecting' ? 'connecting' : 'offline'}`;
    statusText.textContent = status.charAt(0).toUpperCase() + status.slice(1);
    btnConnect.disabled = status === 'connecting';
    btnDisconnect.disabled = status === 'disconnected';
    relayInfo.textContent = status === 'connected' ? `Connected to ${relay.getState().url}` : '';
  });

  // Event log
  relay.onEvent((event) => {
    eventCountEl.textContent = String(relay.getState().eventCount);
    const entry = document.createElement('div');
    entry.className = 'log-entry';
    entry.innerHTML = `<strong>${event.kind}</strong> ${escapeHtml(event.id.slice(0, 16))}... <em>${new Date(event.created_at * 1000).toLocaleTimeString()}</em>`;
    logEntries.prepend(entry);
    // Keep log bounded
    while (logEntries.children.length > 200) {
      logEntries.removeChild(logEntries.lastChild!);
    }
  });

  // Connect button
  btnConnect.addEventListener('click', async () => {
    const url = relayUrlInput.value.trim();
    if (!url) return;
    try {
      await relay.connect(url);
    } catch (err) {
      relayInfo.textContent = `Connection failed: ${err}`;
    }
  });

  // Disconnect button
  btnDisconnect.addEventListener('click', () => {
    relay.disconnect();
    subList.textContent = 'No active subscriptions';
  });

  // Subscribe to memory events
  btnSubMemory.addEventListener('click', () => {
    const subId = relay.subscribeMemoryEvents();
    if (subId) {
      updateSubList();
    }
  });

  function updateSubList(): void {
    const subs = relay.getState().subscriptions;
    if (subs.size === 0) {
      subList.textContent = 'No active subscriptions';
    } else {
      subList.textContent = Array.from(subs.keys()).join('\n');
    }
  }

  // Event inspector
  btnInspect.addEventListener('click', () => {
    const raw = eventJsonInput.value.trim();
    if (!raw) return;
    try {
      const event = JSON.parse(raw) as Event;
      inspectOutput.innerHTML = formatEvent(event);
    } catch (err) {
      inspectOutput.textContent = `Parse error: ${err}`;
    }
  });

  // Clear log
  btnClearLog.addEventListener('click', () => {
    logEntries.innerHTML = '';
  });
}
