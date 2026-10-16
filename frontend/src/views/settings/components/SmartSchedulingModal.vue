<script setup lang="ts">
import type { SmartSchedulingConfig } from '@/api'
import { ZButton, ZDialog, ZFormItem, ZIconButton, ZInput, ZPopover } from '@codex-proxy/ui'
import { CircleHelp } from '@lucide/vue'
import { computed, reactive, shallowRef, useId, watch } from 'vue'

const props = defineProps<{
  config: SmartSchedulingConfig
  defaults: SmartSchedulingConfig
  active: boolean
}>()
const emit = defineEmits<{ confirm: [config: SmartSchedulingConfig] }>()
const open = defineModel<boolean>({ required: true })
const formId = useId()
const fields = [
  { key: 'loadWeight', label: '负载系数', help: '越大越偏向并发占用比例较低的账号' },
  { key: 'quotaWeight', label: '剩余额度系数', help: '越大越偏向剩余额度比例较高的账号' },
  { key: 'healthWeight', label: '健康系数', help: '越大越偏向近期失败率较低的账号' },
  { key: 'latencyWeight', label: '延迟系数', help: '越大越偏向首个有效输出较快的账号' },
  { key: 'resetWeight', label: '额度重置系数', help: '越大越偏向即将重置额度的账号，重置时间未知或已过期时不加分' },
  { key: 'queueWeight', label: '排队系数', help: '需要排队时，越大越偏向等待请求较少的账号，设为 0 时沿用最短队列规则' },
] as const
const draft = reactive({ loadWeight: '', quotaWeight: '', healthWeight: '', latencyWeight: '', resetWeight: '', queueWeight: '' })
const submitted = shallowRef(false)
const errors = computed(() => Object.fromEntries(fields.map(({ key }) => {
  const value = Number(draft[key])
  const valid = draft[key].trim() && Number.isFinite(value) && value >= 0 && value <= 10 && Math.round(value * 10) / 10 === value
  return [key, valid ? undefined : '请输入 0～10 的数值，最多一位小数']
})))
const emptyWeights = computed(() => fields.every(({ key }) => Number(draft[key]) === 0))

function replaceDraft(config: SmartSchedulingConfig) {
  for (const { key } of fields)
    draft[key] = String(config[key])
  submitted.value = false
}

watch(open, (value) => {
  if (value)
    replaceDraft(props.config)
}, { immediate: true })

function confirm() {
  submitted.value = true
  if (Object.values(errors.value).some(Boolean) || emptyWeights.value)
    return
  emit('confirm', {
    loadWeight: Number(draft.loadWeight),
    quotaWeight: Number(draft.quotaWeight),
    healthWeight: Number(draft.healthWeight),
    latencyWeight: Number(draft.latencyWeight),
    resetWeight: Number(draft.resetWeight),
    queueWeight: Number(draft.queueWeight),
  })
  open.value = false
}
</script>

<template>
  <ZDialog v-model="open" title="智能调度设置" description="调整账号选择偏好" width="36rem">
    <form :id="formId" class="grid gap-5" novalidate @submit.prevent="confirm">
      <p v-if="!active" class="m-0 text-cp text-cp-text-secondary">
        仅在启用智能调度后生效
      </p>
      <div>
        <div class="grid gap-4 sm:grid-cols-2">
          <ZFormItem v-for="field in fields" :key="field.key" :error="submitted ? errors[field.key] : undefined">
            <template #label>
              <span class="inline-flex items-center gap-1">
                {{ field.label }}
                <ZPopover trigger="hover-click" placement="top-start">
                  <template #reference="{ open: helpOpen }">
                    <ZIconButton :aria-label="`${field.label}说明`" :title="undefined" :aria-expanded="helpOpen" class="size-6! hover:bg-transparent! active:bg-transparent!">
                      <CircleHelp class="size-3.5" />
                    </ZIconButton>
                  </template>
                  <p class="m-0 max-w-64 px-3 py-2 text-cp-sm leading-relaxed">{{ field.help }}</p>
                </ZPopover>
              </span>
            </template>
            <ZInput v-model="draft[field.key]" type="number" min="0" max="10" step="0.1" placeholder="0 表示不参与评分" :aria-label="field.label" />
          </ZFormItem>
        </div>
        <p v-if="submitted && emptyWeights" role="alert" class="mt-2 mb-0 text-cp-sm text-cp-danger">
          至少一项系数需要大于 0
        </p>
      </div>
    </form>
    <template #footer>
      <ZButton class="mr-auto" @click="replaceDraft(defaults)">
        恢复默认
      </ZButton>
      <ZButton @click="open = false">
        取消
      </ZButton>
      <ZButton native-type="submit" :form="formId" type="primary">
        确定
      </ZButton>
    </template>
  </ZDialog>
</template>
