import { randomUUID, createHash } from 'node:crypto';
import { existsSync, readFileSync, openSync, writeFileSync, fsyncSync, closeSync, linkSync, unlinkSync } from 'node:fs';
import { performance } from 'node:perf_hooks';
import { frameTask, contract } from './node-link.mjs';

// One executor, no automatic reassignment: expired work might still have effects.
export class Mailbox {
  connected = false;
  pending = new Map();
  active;
  receipts = new Map();

  constructor({ marker, send, wake = () => {}, leaseMs = 60_000, taskMs = 120_000, capacity = 32 }) {
    Object.assign(this, { marker, send, wake, leaseMs, taskMs, capacity });
    if (existsSync(marker)) {
      let saved;
      try { saved = JSON.parse(readFileSync(marker, 'utf8')); } catch (error) { if (error.code) throw error; }
      if (typeof saved?.id !== 'string' || typeof saved.lease !== 'string' || typeof saved.claim_id !== 'string') {
        throw new Error('Invalid active-claim marker; verify the old Bot is stopped before removing it');
      }
      this.active = { ...saved, stopped: true };
    }
  }

  setConnected(connected) {
    this.connected = connected;
    if (!connected) {
      this.pending.clear();
      if (this.active) this.active.stopped = true;
    }
  }

  receive(message) {
    this.tick();
    if (message.type === 'cancel') {
      if (this.pending.delete(message.id)) this.send({ type: 'cancelled', id: message.id });
      if (this.active?.id === message.id && !this.active.stopped) this.stopActive();
      return;
    }
    if (this.pending.has(message.id) || this.active?.id === message.id) return;
    if (!this.connected || this.pending.size >= this.capacity) {
      this.send({ type: 'failed', id: message.id });
      return;
    }
    this.pending.set(message.id, { ...message, deadline: performance.now() + this.taskMs });
    // Wake is only a hint. The task and all authority remain in this mailbox.
    this.wake();
  }

  stopActive() {
    this.active.stopped = true;
    // Do not falsely claim that a hosted Bot has stopped running.
    if (this.connected) this.send({ type: 'failed', id: this.active.id });
  }

  tick() {
    const now = performance.now();
    for (const [id, task] of this.pending) {
      if (task.deadline <= now) {
        this.pending.delete(id);
        if (this.connected) this.send({ type: 'failed', id });
      }
    }
    if (this.active && !this.active.stopped &&
        (this.active.deadline <= now || this.active.leaseUntil <= now)) this.stopActive();
  }

  claim(claim_id) {
    this.tick();
    if (this.active) {
      if (!this.active.stopped && this.active.claim_id === claim_id) return this.assignment();
      return { status: 'blocked', reason: 'A prior claim is running or must acknowledge stopping. Do not start another task.' };
    }
    if (!this.connected) return { status: 'offline' };
    const task = this.pending.values().next().value;
    if (!task) return { status: 'empty' };
    const active = { ...task, claim_id, lease: randomUUID(), leaseUntil: performance.now() + this.leaseMs, stopped: false };
    // The marker is written and synced under a private name, then linked into
    // place: linking fails if the marker exists, which fences concurrent bridge
    // processes, and a failed write never leaves a partial marker behind.
    // Sync before exposing work so a crash/restart cannot assign overlapping work.
    const temp = `${this.marker}.${process.pid}.${active.lease}.tmp`;
    try {
      const fd = openSync(temp, 'wx', 0o600);
      try {
        writeFileSync(fd, JSON.stringify({ id: active.id, claim_id, lease: active.lease }));
        fsyncSync(fd);
      } finally { closeSync(fd); }
      linkSync(temp, this.marker);
    } finally {
      try { unlinkSync(temp); } catch {}
    }
    this.active = active;
    this.pending.delete(task.id);
    return this.assignment();
  }

  assignment() {
    const task = this.active;
    return { status: 'claimed', id: task.id, lease: task.lease,
      lease_seconds: this.leaseMs / 1000,
      remaining_seconds: Math.max(0, (task.deadline - performance.now()) / 1000),
      prompt: frameTask(task.instructions, task.text) };
  }

  progress(id, lease) {
    this.tick();
    const task = this.requireClaim(id, lease);
    if (task.stopped || !this.connected) return { status: 'stop_required' };
    task.leaseUntil = performance.now() + this.leaseMs;
    if (!this.send({ type: 'progress', id })) {
      task.stopped = true;
      return { status: 'stop_required' };
    }
    return { status: 'running', lease_seconds: this.leaseMs / 1000 };
  }

  finish(id, lease, outcome, text = '') {
    this.tick();
    const digest = createHash('sha256').update(JSON.stringify([id, outcome, text])).digest('hex');
    const receipt = this.receipts.get(lease);
    if (receipt) {
      if (receipt !== digest) throw new Error('Conflicting terminal result');
      return { status: 'recorded' };
    }
    const task = this.requireClaim(id, lease);
    if (outcome !== 'stopped' && (task.stopped || !this.connected)) {
      return { status: 'stop_required', reason: 'Stop all work, then finish with outcome stopped. Late answers are discarded.' };
    }
    if (outcome === 'result') {
      const line = JSON.stringify({ type: 'result', id, text }) + '\n';
      if (Buffer.byteLength(line) > contract.max_line_bytes) throw new Error('Answer exceeds the agent socket line limit');
    }
    if (!task.stopped && this.connected) {
      if (!this.send(outcome === 'result' ? { type: 'result', id, text } :
        { type: outcome === 'stopped' ? 'cancelled' : 'failed', id })) {
        task.stopped = true;
        return { status: 'stop_required' };
      }
    }
    unlinkSync(this.marker);
    this.active = undefined;
    this.receipts.set(lease, digest);
    if (this.receipts.size > 128) this.receipts.delete(this.receipts.keys().next().value);
    return { status: 'recorded' };
  }

  requireClaim(id, lease) {
    if (!this.active || this.active.id !== id || this.active.lease !== lease) throw new Error('Unknown task or lease');
    return this.active;
  }
}
