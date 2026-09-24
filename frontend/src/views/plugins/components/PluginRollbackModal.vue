<script setup lang="ts">
import type { PluginInstance, PluginRollbackPlan } from '@/api'

import { BaseButton, BaseForm, BaseFormItem, BaseModal, BaseSelect } from '@codex-proxy/ui'

import { computed, shallowRef, watch } from 'vue'
import { shortDigest } from '../utils/model'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = defineProps<{
  instance: PluginInstance | null
  plan: PluginRollbackPlan | null
  loading: boolean
  saving: boolean
}>()

defineEmits<{
  confirm: [artifactSha256: string]
  reload: [instance: PluginInstance]
}>()

const open = defineModel<boolean>({ required: true })
const selected = shallowRef('')
const options = computed(() => (props.plan?.targets ?? []).map(target => ({
  value: target.artifactSha256,
  label: `${target.version} · ${target.platforms.join(', ')}`,
  description: shortDigest(target.artifactSha256),
})))
const canConfirm = computed(() => !props.loading && options.value.some(option => option.value === selected.value))

watch(() => props.plan, () => {
  selected.value = props.plan?.targets[0]?.artifactSha256 ?? ''
})
</script>

<template>
  <BaseModal v-model="open" title="回退插件版本" description="恢复对应版本的设置，保持当前启停状态" size="md" :dismissible="!saving">
    <BaseForm class="grid gap-4">
      <BaseFormItem label="当前版本">
        <p class="m-0 break-all text-cp-sm text-cp-text">
          {{ instance?.name }} <span v-if="plan" class="font-mono">· {{ plan.currentVersion }}</span>
        </p>
      </BaseFormItem>
      <BaseFormItem v-if="loading || options.length" label="目标版本" required>
        <template #label-extra>
          <PluginHelpPopover label="回退说明">
            <p class="m-0">
              恢复目标版本上次启用时的参数和密钥，私有数据仍须通过兼容检查
            </p>
            <p v-if="selected" class="m-0 break-all font-mono">
              SHA-256 {{ selected }}
            </p>
          </PluginHelpPopover>
        </template>
        <BaseSelect
          v-model="selected"
          class="w-full"
          :options="options"
          :disabled="loading || saving"
          :placeholder="loading ? '正在加载旧版' : '请选择回退目标'"
          aria-label="回退目标版本"
        />
      </BaseFormItem>
      <p v-else-if="plan" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
        没有保留恢复设置的旧版，其他版本可在“版本”中切换
      </p>
    </BaseForm>
    <template #footer>
      <BaseButton v-if="instance" variant="secondary" :disabled="saving" :loading="loading" @click="$emit('reload', instance)">
        重新加载
      </BaseButton>
      <BaseButton variant="secondary" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="primary" :disabled="!canConfirm" :loading="saving" @click="$emit('confirm', selected)">
        确认回退
      </BaseButton>
    </template>
  </BaseModal>
</template>
