<script setup lang="ts">
import type { ApiKey, KeyAlignmentPreview } from '@/api'
import { ZButton, ZSelect } from '@codex-proxy/ui'
import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { alignKeyBudget, previewKeyAlignment } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useRequestState } from '@/composables/useRequestState'
import { notification } from '@/plugins/notification'

import KeyReferenceAccountPicker from './KeyReferenceAccountPicker.vue'

const props = defineProps<{ apiKey: ApiKey }>()
const emit = defineEmits<{ saved: [], busy: [value: boolean] }>()
const { t } = useI18n({ useScope: 'global' })
const accountId = shallowRef<string>()
const windowKey = shallowRef<string>()
const preview = shallowRef<KeyAlignmentPreview>()
const request = useRequestState()
const { error: previewError, loading: previewLoading } = request
const action = useAsyncAction()
const selectedWindow = computed(() => preview.value?.windows.find(item => item.key === windowKey.value))
const options = computed(() => preview.value?.windows.map(item => ({
  value: item.key,
  label: [
    item.period === 'daily'
      ? t('keys.budgetReset.dailyQuota')
      : t('keys.budgetReset.weeklyQuota'),
    item.label,
  ].filter(Boolean).join(' · '),
})) ?? [])
async function loadPreview() {
  preview.value = undefined
  windowKey.value = undefined
  const account = accountId.value
  if (!account) {
    request.invalidate()
    previewError.value = ''
    return
  }
  const id = request.start()
  try {
    const result = await previewKeyAlignment(props.apiKey.id, account, { signal: request.signal, silent: true })
    if (request.isCurrent(id)) {
      preview.value = result
      windowKey.value = result.windows[0]?.key
    }
  }
  catch (cause) {
    request.fail(id, cause)
  }
  finally {
    request.finish(id)
  }
}
watch(accountId, loadPreview, { flush: 'sync' })
watch(action.loading, value => emit('busy', value), { flush: 'sync' })
async function submit() {
  const window = selectedWindow.value
  const account = accountId.value
  const revision = preview.value?.revision
  if (!window || !account || revision === undefined)
    return
  await action.run(async () => {
    await alignKeyBudget({
      id: props.apiKey.id,
      accountId: account,
      revision,
      windowKey: window.key,
      startsAt: window.startsAt,
      endsAt: window.endsAt,
    })
    notification.success({
      message: t('keys.alignment.saved'),
    })
    emit('saved')
  }, {
    onError: () => void loadPreview(),
  })
}
defineExpose({
  canSubmit: computed(() => !!selectedWindow.value),
  submit,
})
</script>

<template>
  <div class="grid gap-4">
    <div class="grid gap-1.5">
      <span class="text-z-sm text-z-text-secondary">{{ t('keys.alignment.account') }}</span>
      <KeyReferenceAccountPicker v-model="accountId" :key-id="apiKey.id" :disabled="action.loading.value" />
    </div>
    <span v-if="previewLoading" role="status" class="text-z-sm text-z-text-tertiary">{{ t('common.status.loading') }}</span>
    <div v-else-if="previewError" role="status" class="flex flex-wrap items-center justify-between gap-2 text-z-sm">
      {{ previewError }} <ZButton size="small" @click="loadPreview">
        {{ t('common.actions.refresh') }}
      </ZButton>
    </div>
    <span v-else-if="preview && !options.length" role="status" class="text-z-sm text-z-text-tertiary">{{ t('keys.alignment.empty') }}</span>
    <div v-else-if="options.length" class="grid gap-1.5">
      <span class="text-z-sm text-z-text-secondary">{{ t('keys.alignment.window') }}</span>
      <ZSelect v-model="windowKey" :options="options" :disabled="action.loading.value" :aria-label="t('keys.alignment.window')" />
      <dl v-if="selectedWindow" class="my-1 grid grid-cols-[max-content_1fr] gap-x-4 gap-y-2 rounded-z bg-z-fill-quaternary p-3.5 text-z-sm">
        <dt>{{ t('keys.alignment.start') }}</dt><dd class="m-0 text-right tabular-nums">
          {{ selectedWindow.startsAtDisplay }}
        </dd>
        <dt>{{ t('keys.alignment.end') }}</dt><dd class="m-0 text-right tabular-nums">
          {{ selectedWindow.endsAtDisplay }}
        </dd>
      </dl>
    </div>
  </div>
</template>
