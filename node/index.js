/**
 * pdast — PureData .pd patch parser
 *
 * Usage:
 *   import { parse, emitPatch } from '@konsumer/pdast';
 *   const result = parse(pdFileContent);
 */

import wasmInit, { parse, parseToJson, emitPatch, emitPatchFromJson, wclapToC } from '../pdast/pkg/pdast.js'
export { parse, parseToJson, emitPatch, emitPatchFromJson, wclapToC }

let _init
export const init = (url) => (_init ??= wasmInit(url))
export default init
