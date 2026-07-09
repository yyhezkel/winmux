// Phase 78: convert a Claude "/usage" reset string into the viewer's LOCAL
// timezone. The CLI reports resets in the account's home zone, e.g.
// "Jul 8, 4:10am (Europe/Berlin)" — which is confusing when the user's machine
// is in a different zone. We parse the wall-clock + IANA zone and re-express it
// in the local zone. Pure frontend (Intl has full ICU tz data in WebView2); no
// backend change, no new dependency.

const MONTHS: Record<string, number> = {
  Jan: 0, Feb: 1, Mar: 2, Apr: 3, May: 4, Jun: 5,
  Jul: 6, Aug: 7, Sep: 8, Oct: 9, Nov: 10, Dec: 11,
};

/** Offset (ms) of `timeZone` at the instant `utcMs`, i.e. localWall - utc. */
function zoneOffsetMs(utcMs: number, timeZone: string): number {
  const dtf = new Intl.DateTimeFormat("en-US", {
    timeZone,
    hour12: false,
    year: "numeric",
    month: "numeric",
    day: "numeric",
    hour: "numeric",
    minute: "numeric",
    second: "numeric",
  });
  const m: Record<string, number> = {};
  for (const part of dtf.formatToParts(new Date(utcMs))) {
    if (part.type !== "literal") m[part.type] = Number(part.value);
  }
  const asUtc = Date.UTC(m.year, m.month - 1, m.day, m.hour % 24, m.minute, m.second);
  return asUtc - utcMs;
}

/** Epoch ms for a wall-clock time interpreted in `timeZone` (DST-correct). */
function wallToUnixMs(
  y: number,
  mon: number,
  d: number,
  h: number,
  mi: number,
  timeZone: string,
): number {
  const guess = Date.UTC(y, mon, d, h, mi);
  const off1 = zoneOffsetMs(guess, timeZone);
  let utc = guess - off1;
  // One refinement pass covers the DST-boundary case where the offset at the
  // guessed instant differs from the offset at the true instant.
  const off2 = zoneOffsetMs(utc, timeZone);
  if (off2 !== off1) utc = guess - off2;
  return utc;
}

export interface ParsedReset {
  /** Absolute epoch ms of the reset. */
  ms: number;
  /** Formatted in the viewer's local zone (e.g. "8 Jul, 05:10"). */
  local: string;
  /** The source IANA zone from the CLI, e.g. "Europe/Berlin". */
  sourceTz: string;
}

/**
 * Parse `"Jul 8, 4:10am (Europe/Berlin)"` → the same instant in the local zone.
 * `anchorUnix` (seconds) is used to pick the year (resets are in the future).
 * Returns null if the string doesn't match the expected shape.
 */
export function parseReset(
  reset: string,
  anchorUnix: number,
  locale?: string,
): ParsedReset | null {
  const tz = reset.match(/\(([^)]+)\)\s*$/);
  const timeZone = tz?.[1]?.trim();
  if (!timeZone || tz?.index === undefined) return null;

  const body = reset.slice(0, tz.index).trim(); // "Jul 8, 4:10am"
  const m = body.match(/^([A-Za-z]{3})\s+(\d{1,2}),?\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)$/i);
  if (!m) return null;

  const mon = MONTHS[m[1][0].toUpperCase() + m[1].slice(1, 3).toLowerCase()];
  if (mon === undefined) return null;
  const day = Number(m[2]);
  let hour = Number(m[3]) % 12;
  if (/pm/i.test(m[5])) hour += 12;
  const min = m[4] ? Number(m[4]) : 0;

  // Choose the year so the reset lands at/after the anchor (allow a small slack
  // so a just-passed reset near "now" still resolves to this year, and the
  // Dec→Jan rollover falls through to next year).
  const anchorMs = anchorUnix * 1000;
  const anchorYear = new Date(anchorMs).getUTCFullYear();
  let ms = wallToUnixMs(anchorYear, mon, day, hour, min, timeZone);
  if (ms < anchorMs - 36 * 3600_000) {
    ms = wallToUnixMs(anchorYear + 1, mon, day, hour, min, timeZone);
  }

  let local: string;
  try {
    local = new Date(ms).toLocaleString(locale, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    local = new Date(ms).toLocaleString();
  }
  return { ms, local, sourceTz: timeZone };
}

/**
 * Reset string re-expressed in the local zone, e.g. "8 Jul, 05:10".
 * Falls back to the raw CLI string if it can't be parsed.
 */
export function formatResetLocal(
  reset: string,
  anchorUnix: number,
  locale?: string,
): string {
  return parseReset(reset, anchorUnix, locale)?.local ?? reset;
}
