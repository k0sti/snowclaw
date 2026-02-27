/**
 * Relay connection management using nostr-tools.
 *
 * Handles WebSocket connections to Nostr relays and subscription lifecycle.
 */

import { Relay, type Sub, type Event } from 'nostr-tools';

export type RelayStatus = 'disconnected' | 'connecting' | 'connected';

export interface RelayState {
  url: string;
  relay: Relay | null;
  status: RelayStatus;
  subscriptions: Map<string, Sub>;
  eventCount: number;
}

const state: RelayState = {
  url: '',
  relay: null,
  status: 'disconnected',
  subscriptions: new Map(),
  eventCount: 0,
};

type StatusListener = (status: RelayStatus) => void;
type EventListener = (event: Event) => void;

const statusListeners: StatusListener[] = [];
const eventListeners: EventListener[] = [];

export function onStatusChange(fn: StatusListener): void {
  statusListeners.push(fn);
}

export function onEvent(fn: EventListener): void {
  eventListeners.push(fn);
}

function setStatus(s: RelayStatus): void {
  state.status = s;
  for (const fn of statusListeners) fn(s);
}

export function getState(): Readonly<RelayState> {
  return state;
}

export async function connect(url: string): Promise<void> {
  if (state.relay) {
    state.relay.close();
  }
  state.url = url;
  setStatus('connecting');

  try {
    const relay = new Relay(url);
    await relay.connect();
    state.relay = relay;
    setStatus('connected');
  } catch (err) {
    setStatus('disconnected');
    throw err;
  }
}

export function disconnect(): void {
  if (state.relay) {
    // Close all subscriptions first
    for (const [id, sub] of state.subscriptions) {
      sub.close();
      state.subscriptions.delete(id);
    }
    state.relay.close();
    state.relay = null;
  }
  setStatus('disconnected');
}

/** Subscribe to NIP-78 memory events (kind 30078 with snow: d-tag prefix). */
export function subscribeMemoryEvents(): string | null {
  if (!state.relay) return null;

  const subId = `snow-mem-${Date.now()}`;
  const sub = state.relay.subscribe(
    [
      {
        kinds: [30078],
        '#d': ['snow:memory:'],
      },
    ],
    {
      onevent(event: Event) {
        state.eventCount++;
        for (const fn of eventListeners) fn(event);
      },
      oneose() {
        // End of stored events — subscription stays open for real-time
      },
    },
  );

  state.subscriptions.set(subId, sub);
  return subId;
}

export function unsubscribe(subId: string): void {
  const sub = state.subscriptions.get(subId);
  if (sub) {
    sub.close();
    state.subscriptions.delete(subId);
  }
}
