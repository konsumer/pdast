/**
 * web/wclap.js — main-thread wrapper around the WCLAP build worker.
 *
 * `compileWclap` spawns `wclap-worker.js` (which loads the browsercc LLVM
 * toolchain from a CDN and compiles there, off the UI thread) and resolves
 * with the linked plugin bytes.
 *
 * @param {{ cSource: string, pluginId: string, pluginName: string }} job
 * @param {(message: string) => void} [onProgress]
 * @returns {Promise<{ wasm: Uint8Array, output: string }>}
 */
export function compileWclap(job, onProgress) {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./wclap-worker.js', import.meta.url), { type: 'module' })

    const finish = (fn, value) => {
      worker.terminate()
      fn(value)
    }

    worker.addEventListener('message', (event) => {
      const { type } = event.data
      if (type === 'progress') {
        onProgress?.(event.data.message)
      } else if (type === 'done') {
        finish(resolve, { wasm: new Uint8Array(event.data.wasm), output: event.data.output })
      } else if (type === 'error') {
        finish(reject, new Error(event.data.message))
      }
    })

    worker.addEventListener('error', (event) => {
      finish(reject, new Error(event.message || 'WCLAP worker failed to start'))
    })

    worker.postMessage(job)
  })
}
