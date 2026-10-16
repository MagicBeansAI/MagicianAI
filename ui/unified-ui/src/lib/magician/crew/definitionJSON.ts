/** Definitions can contain Rust u64 values beyond JavaScript's safe integers.
 * Tokenize outside JSON strings so an unrelated edit never rounds them. */
const tokens = /"(?:\\.|[^"\\])*"|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g;
export function parseDefinitionJSON(source: string): unknown {
 JSON.parse(source); // Validate the original grammar before replacing any token.
 let prefix = '__definition_integer__';
 while (source.includes(prefix)) prefix += '_';
 const integers = new Map<string,bigint>();
 const protectedSource = source.replace(tokens, token => {
  if (token.startsWith('"') || !/^-?\d+$/.test(token) || Number.isSafeInteger(Number(token))) return token;
  const key = `${prefix}${integers.size}`;
  integers.set(key,BigInt(token));
  return JSON.stringify(key);
 });
 return JSON.parse(protectedSource,(_key,value) => typeof value === 'string' && integers.has(value) ? integers.get(value) : value);
}
export function stringifyDefinitionJSON(value: unknown, space?: number): string {
 const strings = new Set<string>();
 function collect(input: unknown) {
  if (typeof input === 'string') strings.add(input);
  else if (Array.isArray(input)) input.forEach(collect);
  else if (input && typeof input === 'object') for (const [key,child] of Object.entries(input)) {strings.add(key);collect(child);}
 }
 collect(value);
 const integers = new Map<string,string>(); let index = 0;
 const result = JSON.stringify(value,(_key,child) => {
  if (typeof child !== 'bigint') return child;
  let key: string;
  do {key = `__definition_integer__${index++}`;} while (strings.has(key));
  integers.set(key,String(child)); return key;
 },space);
 return result?.replace(tokens,token => token.startsWith('"') ? integers.get(JSON.parse(token)) ?? token : token) ?? 'null';
}
