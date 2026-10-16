/**
 * Names from Linux as Linux has them. WSL hands a Linux name holding a character Windows forbids
 * in names (`\ : * ? " < > |`, or a control character) to Windows with that character moved into
 * the private-use area, at U+F000 plus its code; shown as they are they look like boxes.
 */

const ENCODED = /[-]/;
const FORBIDDEN = new Set(['\\', ":", "*", "?", '"', "<", ">", "|"]);

/** A name as Linux spells it: WSL's private-use stand-ins turned back into their characters. */
export function linuxName(name: string): string {
  if (!ENCODED.test(name)) return name;
  let out = "";
  for (const char of name) {
    const code = char.codePointAt(0) ?? 0;
    const original = code >= 0xf001 && code <= 0xf07f ? String.fromCodePoint(code - 0xf000) : null;
    out += original !== null && (FORBIDDEN.has(original) || code - 0xf000 < 0x20) ? original : char;
  }
  return out;
}
