/** Small decoders shared by HTTP and WebSocket boundaries; never trust parsed JSON. */
export type Decoder<T> = (value: unknown) => T;
export type Fields<T> = { [K in keyof T]-?: Decoder<T[K]> };

export function invalid(): never {
  throw new Error('Invalid response data');
}
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
export function record(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : invalid();
}
export function text(value: unknown): string {
  return typeof value === 'string' ? value : invalid();
}
export function boolean(value: unknown): boolean {
  return typeof value === 'boolean' ? value : invalid();
}
export function number(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : invalid();
}
export function integer(max = Number.MAX_SAFE_INTEGER, min = 0): Decoder<number> {
  return (value) => {
    const result = number(value);
    return Number.isSafeInteger(result) && result >= min && result <= max ? result : invalid();
  };
}
export function choice<const T extends string>(...values: T[]): Decoder<T> {
  return (value) => {
    for (const candidate of values) if (value === candidate) return candidate;
    return invalid();
  };
}
/** Normalize Rust optional wire fields; nullable() instead preserves an explicit null. */
export function optional<T>(decode: Decoder<T>): Decoder<T | undefined> {
  return (value) => (value === undefined || value === null ? undefined : decode(value));
}
export function nullable<T>(decode: Decoder<T>): Decoder<T | null> {
  return (value) => (value === null ? null : decode(value));
}
export function list<T>(decode: Decoder<T>): Decoder<T[]> {
  return (value) =>
    Array.isArray(value) ? value.map((entry: unknown) => decode(entry)) : invalid();
}
export function object<T extends object>(fields: Fields<T>): Decoder<T> {
  return (value) => {
    const source = record(value);
    const result: Record<string, unknown> = {};
    for (const key in fields) {
      const decoded: unknown = fields[key](
        Object.prototype.hasOwnProperty.call(source, key) ? source[key] : undefined,
      );
      if (decoded !== undefined)
        Object.defineProperty(result, key, {
          value: decoded,
          enumerable: true,
          writable: true,
          configurable: true,
        });
    }
    // Fields<T> requires a decoder for every property. Only decoded own fields
    // survive, so unknown JSON properties cannot reach application/library code.
    return result as T;
  };
}
