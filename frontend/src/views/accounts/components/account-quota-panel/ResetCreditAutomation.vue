<script setup lang="ts">
import type { AccountResetCreditAutomation } from '@/api'
import { ZButton, ZIconButton, ZPopover, ZSwitch } from '@codex-proxy/ui'
import { CircleHelp, Clock3 } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { useRouter } from 'vue-router'
import { getAccountResetCreditAutomation, setAccountResetCreditAutomation } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { errorMessage } from '@/utils/operation'

const props = defineProps<{ accountId: string }>()
const router = useRouter()
const status = shallowRef<AccountResetCreditAutomation>()
const loadError = shallowRef('')
const switchEnabled = shallowRef(false)
const loadAction = useAsyncAction({ errorText: false })
const saveAction = useAsyncAction()
const busy = computed(() => loadAction.loading.value || saveAction.loading.value)
const resultText = computed(() => {
  switch (status.value?.lastResult) {
    case 'pending': return status.value?.enabled ? '上次自动重置结果待确认，将继续核对' : '上次自动重置结果待确认'
    case 'expired_unknown': return '上次重置结果未能确认，卡片已到期'
    case 'discovery_failed': return '最近查询失败，将按计划重试'
    case 'not_redeemed': return '上游未完成最近一次自动重置'
    default: return ''
  }
})

async function load() {
  const accountId = props.accountId
  loadError.value = ''
  await loadAction.run(async () => {
    try {
      const result = await getAccountResetCreditAutomation({ accountId }, { silent: true })
      if (props.accountId === accountId) {
        status.value = result
        switchEnabled.value = result.enabled
      }
    }
    catch (error) {
      if (props.accountId === accountId)
        loadError.value = errorMessage(error)
    }
  })
}

async function toggle(enabled: boolean) {
  const accountId = props.accountId
  switchEnabled.value = enabled
  await saveAction.run(async () => {
    const result = await setAccountResetCreditAutomation({ accountId, enabled })
    if (props.accountId === accountId)
      status.value = result
  })
  if (props.accountId === accountId)
    switchEnabled.value = status.value?.enabled ?? false
}

watch(() => props.accountId, () => {
  status.value = undefined
  switchEnabled.value = false
  void load()
}, { immediate: true })
</script>

<template>
  <section class="grid gap-2 rounded-cp bg-cp-fill-quaternary px-4 py-3" aria-label="自动使用重置卡">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <div class="flex items-center gap-1">
        <h3 class="m-0 text-cp-sm font-heavy text-cp-text">
          自动使用
        </h3>
        <ZPopover trigger="click" placement="bottom-start" role="dialog">
          <template #reference="{ open, id }">
            <ZIconButton size="small" class="size-6! p-0!" aria-label="自动使用规则" :aria-expanded="open" :aria-controls="id">
              <CircleHelp class="size-3.5" />
            </ZIconButton>
          </template>
          <div class="grid max-w-64 gap-2 p-3 text-cp-xs leading-relaxed text-cp-text-secondary">
            <p>每天随机查询一次，各账号错开时间</p>
            <p>临期使用前核对卡片，已在其他地方使用的会自动跳过</p>
          </div>
        </ZPopover>
      </div>
      <div class="flex items-center gap-3">
        <ZButton size="small" variant="plain" aria-label="设置全局提前时间" @click="router.push('/settings#reset-credits')">
          <template #icon>
            <Clock3 class="size-3.5 -translate-y-px" />
          </template>
          {{ status ? `提前 ${status.leadMinutes} 分钟` : '提前时间' }}
        </ZButton>
        <ZSwitch
          class="reset-credit-switch"
          :model-value="switchEnabled"
          :disabled="busy || !status"
          aria-label="自动使用临期重置卡"
          @update:model-value="toggle"
        />
      </div>
    </div>
    <p v-if="status?.enabled && status.nextConsumeAtDisplay" class="text-cp-xs text-cp-text-secondary">
      下次使用 {{ status.nextConsumeAtDisplay }}
    </p>
    <p v-if="resultText" class="text-cp-xs text-cp-warning" role="status">
      {{ resultText }}
    </p>
    <div v-if="loadError" class="flex items-center justify-between gap-2 text-cp-xs text-cp-error" role="alert">
      <span>{{ loadError }}</span>
      <ZButton size="small" variant="plain" :loading="loadAction.loading.value" @click="load">
        重试
      </ZButton>
    </div>
  </section>
</template>

<style scoped>
.reset-credit-switch :deep(input:not(:checked) + span) {
  background-color: var(--cp-switch-unchecked-bg);
}
</style>
