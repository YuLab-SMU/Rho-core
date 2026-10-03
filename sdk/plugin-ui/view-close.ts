import type { PluginViewLifecycle, PluginViewRequest } from "../plugin-protocol/index.js";

export interface ViewCloseHandler {
  /** Complete owner-defined preparation through declared ports. Returning does
   * not ask Core to verify content or cancel accepted scientific work. */
  prepare(): Promise<void>;
  resume?(): void;
}
export interface ViewCloseSnapshot { preparing: boolean; operation: string | null; error: string; }
export interface ViewClosePort {
  view: string;
  request<T>(body: PluginViewRequest): Promise<T>;
}

/** One document can acknowledge only its own preparation. An ended channel retains
 * uncertain preparation; disposal is never evidence that a draft was saved. */
export class ViewCloseCooperation {
  private readonly renderer = crypto.randomUUID();
  private snapshot: ViewCloseSnapshot = { preparing: false, operation: null, error: "" };
  private listeners = new Set<() => void>();
  private timer: ReturnType<typeof setTimeout> | undefined;
  private observing: Promise<void> | null = null;
  private stopped = false;
  constructor(private port: ViewClosePort, private handler: ViewCloseHandler) {}
  getSnapshot = (): ViewCloseSnapshot => this.snapshot;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  private publish(next: ViewCloseSnapshot) { this.snapshot = next; for (const listener of this.listeners) listener(); }
  async start(): Promise<void> {
    await this.port.request({ type: "register_close_handler", renderer: this.renderer });
    if (this.stopped) return;
    this.schedule();
  }
  private schedule() {
    if (!this.stopped) this.timer = setTimeout(() => { void this.observe().catch(() => undefined); }, 250);
  }
  /** Also useful when a container becomes visible; concurrent observations join
   * the same attempt and cannot prepare or acknowledge a close twice. */
  observe(): Promise<void> {
    if (this.observing) return this.observing;
    if (this.stopped) return Promise.resolve();
    clearTimeout(this.timer);
    const task = this.poll().catch(error => {
      if (!this.stopped) {
        this.publish({ ...this.snapshot, error: `Close confirmation unavailable: ${error instanceof Error ? error.message : String(error)}` });
        // A broken channel cannot establish whether native closure committed.
        this.stopped = true;
      }
      throw error;
    }).finally(() => { this.observing = null; this.schedule(); });
    this.observing = task;
    return task;
  }
  private async poll() {
    const lifecycle = await this.port.request<PluginViewLifecycle>({ type: "observe_lifecycle", renderer: this.renderer });
    if (this.stopped) return;
    if (lifecycle.view !== this.port.view || !lifecycle.close) throw new Error("View lifecycle identity changed.");
    const close = lifecycle.close;
    if (close.phase === "open") {
      if (this.snapshot.preparing) {
        this.handler.resume?.();
        this.publish({ ...this.snapshot, preparing: false, operation: null });
      }
      return;
    }
    if (close.phase === "refused") {
      this.publish({ ...this.snapshot, error: close.reason });
      return;
    }
    if (close.phase !== "requested" || this.snapshot.operation === close.operation) return;
    if (this.snapshot.preparing) this.handler.resume?.();
    this.publish({ preparing: true, operation: close.operation, error: "" });
    try {
      await this.handler.prepare();
      if (this.stopped) return;
    } catch (error) {
      if (this.stopped) return;
      const reason = error instanceof Error ? error.message : String(error);
      this.publish({ ...this.snapshot, error: `Close preparation failed: ${reason}` });
      // Limit by UTF-8 bytes, without cutting a multibyte character.
      let bounded = "";
      for (const char of reason || "Owner preparation failed.") {
        if (new TextEncoder().encode(bounded + char).length > 4096) break;
        bounded += char;
      }
      await this.port.request({ type: "refuse_close", renderer: this.renderer, operation: close.operation, reason: bounded }).catch(() => undefined);
      return;
    }
    // A lost acknowledgement can follow a committed close. Keep uncertainty
    // distinct from a preparation refusal; the container owns the original Operation.
    try { await this.port.request({ type: "prepare_close", renderer: this.renderer, operation: close.operation }); }
    catch (error) {
      if (!this.stopped) this.publish({ ...this.snapshot, error: `Close acknowledgement is unconfirmed: ${error instanceof Error ? error.message : String(error)}` });
    }
  }
  dispose() {
    this.stopped = true; clearTimeout(this.timer);
    this.listeners.clear();
  }
}
