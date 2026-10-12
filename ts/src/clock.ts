// A monotonic time source. Native Rust uses `std::time::Instant`; here
// `Date.now()` backs the standard clock.

export interface Clock {
  /** Seconds since the clock's creation (monotonic-ish). */
  nowSecs(): number;
}

export class StdClock implements Clock {
  private readonly t0: number;
  constructor() {
    this.t0 = Date.now();
  }
  nowSecs(): number {
    return (Date.now() - this.t0) / 1000;
  }
}
