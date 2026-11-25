import type { Ref } from 'vue'
import type { RequestProfilePreview } from '@/api'
import { isEqual } from 'es-toolkit'
import { computed, shallowRef, watch } from 'vue'
import { getRequestProfileOptions, previewRequestProfile } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { notification } from '@/plugins/notification'
import { errorMessage } from '@/utils/operation'

export interface ProfileEditorProps<Selection> {
  active: boolean
  disabled: boolean
  allowInherit: boolean
  allowPreserve: boolean
  draft?: Selection
}

export function useProfileEditor<
  Selection extends object,
  Options extends { globalConfiguration: Selection },
  Preview extends RequestProfilePreview,
>(
  provider: string,
  model: Ref<Selection | null | undefined>,
  props: ProfileEditorProps<Selection>,
  needsInput: (configuration: Selection, options: Options) => boolean,
  showPreview: (configuration: Selection) => boolean = () => true,
) {
  const options = shallowRef<Options>()
  const optionsRequest = useRequestState()
  const previewRequest = useRequestState(error => notification.error({
    message: errorMessage(error),
    deduplicate: true,
  }))
  const effective = computed(() => model.value ?? options.value?.globalConfiguration)
  const previewInput = computed(() => options.value && model.value !== undefined
    ? { configuration: model.value, globalConfiguration: options.value.globalConfiguration }
    : undefined)
  const previewResult = shallowRef<{ input: NonNullable<typeof previewInput.value>, data: Preview }>()
  const preview = computed(() => {
    const result = previewResult.value
    if (!result || !isEqual(result.input, previewInput.value))
      return undefined
    return result.data
  })
  const incomplete = computed(() => !!effective.value && !!options.value && needsInput(effective.value, options.value))
  const inherited = computed(() => props.allowInherit && model.value === null)
  const source = computed({
    get: () => model.value === undefined
      ? 'keep'
      : model.value === null
        ? 'global'
        : 'independent',
    set: (value: string) => {
      if (value === 'keep') {
        model.value = undefined
      }
      else if (value === 'global') {
        model.value = null
      }
      else {
        const selection = props.draft ?? effective.value
        if (selection)
          model.value = { ...selection }
      }
    },
  })
  const frame = computed(() => ({
    allowInherit: props.allowInherit,
    allowPreserve: props.allowPreserve,
    sourceDisabled: props.disabled || optionsRequest.loading.value || !options.value,
    loadError: optionsRequest.error.value,
    showFields: model.value !== undefined && !inherited.value,
    showPreview: model.value !== undefined && (inherited.value || !effective.value || showPreview(effective.value)),
    preview: preview.value,
    previewing: optionsRequest.loading.value || previewRequest.loading.value,
    needsInput: incomplete.value,
  }))

  watch(() => props.active, async (active, _, onCleanup) => {
    if (!active)
      return
    if (props.allowInherit) {
      options.value = undefined
      previewResult.value = undefined
    }
    const requestId = optionsRequest.start()
    onCleanup(() => optionsRequest.invalidate())
    try {
      const result = await getRequestProfileOptions<Options>(provider, { signal: optionsRequest.signal })
      if (optionsRequest.isCurrent(requestId))
        options.value = result
    }
    catch (error) {
      optionsRequest.fail(requestId, error)
    }
    finally {
      optionsRequest.finish(requestId)
    }
  }, { immediate: true })

  const readyToPreview = computed(() => props.active && !optionsRequest.loading.value && !optionsRequest.error.value && !incomplete.value)
  watch([readyToPreview, previewInput], ([ready, input], _, onCleanup) => {
    if (!ready || !input)
      return
    const requestId = previewRequest.start()
    const timer = setTimeout(async () => {
      try {
        const result = await previewRequestProfile<Preview>(provider, input.configuration, { signal: previewRequest.signal })
        if (previewRequest.isCurrent(requestId))
          previewResult.value = { input, data: result }
      }
      catch (error) {
        previewRequest.fail(requestId, error)
      }
      finally {
        previewRequest.finish(requestId)
      }
    }, 250)
    onCleanup(() => {
      clearTimeout(timer)
      previewRequest.invalidate()
    })
  }, { immediate: true })

  return {
    options,
    preview,
    effective,
    frame,
    source,
    loading: optionsRequest.loading,
    previewing: previewRequest.loading,
  }
}
