import { expect, it } from 'vitest';
import { parseDefinitionJSON, stringifyDefinitionJSON } from './definitionJSON';
import { serializeDefinitionYaml } from './definitionApi';
it('preserves u64 fields, nested integers and integer-looking strings through editing and YAML', () => {
 const source = '{"definition":{"agent_id":"owner","limit":18446744073709551615,"negative":-9007199254740993,"source":"18446744073709551615","nested":[9007199254740993,3,0.5],"__definition_integer__0":"unchanged"},"version":1}';
 const parsed = parseDefinitionJSON(source) as {definition:Record<string,unknown>;version:number};
 expect(parsed.definition.limit).toBe(18446744073709551615n); expect(parsed.definition.source).toBe('18446744073709551615');
 const encoded = stringifyDefinitionJSON(parsed);
 expect(parseDefinitionJSON(encoded)).toEqual(parsed);
 expect(encoded).toContain('"limit":18446744073709551615');
 expect(encoded).toContain('"__definition_integer__0":"unchanged"');
 expect(serializeDefinitionYaml(parsed.definition)).toContain('"limit": 18446744073709551615');
 expect(serializeDefinitionYaml(parsed.definition)).toContain('"source": "18446744073709551615"');
 expect(() => parseDefinitionJSON('{"limit":018446744073709551615}')).toThrow();
});
