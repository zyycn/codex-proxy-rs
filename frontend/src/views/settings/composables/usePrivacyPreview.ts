import type { CodexPrivacyPolicy, PrivacyPreviewResult, PrivacySample } from '@/api/modules/settings/privacy'
import { shallowRef } from 'vue'
import { previewPrivacyPolicy } from '@/api/modules/settings/privacy'
import { useRequestState } from '@/composables/useRequestState'

export function usePrivacyPreview() {
  const state = useRequestState()
  const result = shallowRef<PrivacyPreviewResult>()
  function invalidate() {
    state.invalidate()
    result.value = undefined
    state.error.value = ''
  }
  async function run(policy: CodexPrivacyPolicy, sample: PrivacySample) {
    const id = state.start()
    result.value = undefined
    try {
      const response = await previewPrivacyPolicy({ policy, ...sample }, { signal: state.signal })
      if (!state.isCurrent(id))
        return undefined
      result.value = response
      return response
    }
    catch (cause) { state.fail(id, cause) }
    finally { state.finish(id) }
  }
  return { loading: state.loading, error: state.error, result, run, invalidate }
}
