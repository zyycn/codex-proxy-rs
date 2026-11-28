<script setup lang="ts">
import type { PluginInstance, PluginRollbackPlan } from '@/api'

import { ZButton, ZDialog, ZForm, ZFormItem, ZSelect } from '@codex-proxy/ui'

import { computed, shallowRef, watch } from 'vue'

import { useI18n } from 'vue-i18n'
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

const { t } = useI18n({ useScope: 'global' })

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
  <ZDialog v-model="open" destroy-on-close :title="t('plugins.rollback.rollBackPluginVersion')" :description="t('plugins.rollback.description')" width="36rem" :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving">
    <ZForm class="grid gap-4">
      <ZFormItem :label="t('plugins.catalog.currentVersion')">
        <p class="m-0 break-all text-z-sm text-z-text">
          {{ instance?.name }} <span v-if="plan" class="font-mono">· {{ plan.currentVersion }}</span>
        </p>
      </ZFormItem>
      <ZFormItem v-if="loading || options.length" :label="t('plugins.rollback.targetVersion')" required>
        <template #label-extra>
          <PluginHelpPopover :label="t('plugins.rollback.fallbackInstructions')">
            <p class="m-0">
              {{ t('plugins.rollback.restoreHelp') }}
            </p>
            <p v-if="selected" class="m-0 break-all font-mono">
              SHA-256 {{ selected }}
            </p>
          </PluginHelpPopover>
        </template>
        <ZSelect
          v-model="selected"
          class="w-full"
          :options="options"
          :disabled="loading || saving"
          :placeholder="loading ? t('plugins.rollback.loadingOlderVersions') : t('plugins.rollback.selectRollbackVersion')"
          :aria-label="t('plugins.rollback.rollBackToTargetVersion')"
        />
      </ZFormItem>
      <p v-else-if="plan" role="status" class="m-0 text-z-sm text-z-text-secondary">
        {{ t('plugins.rollback.empty') }}
      </p>
    </ZForm>
    <template #footer>
      <ZButton v-if="instance" :disabled="saving" :loading="loading" @click="$emit('reload', instance)">
        {{ t('plugins.rollback.reload') }}
      </ZButton>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :disabled="!canConfirm" :loading="saving" @click="$emit('confirm', selected)">
        {{ t('plugins.rollback.confirmRollback') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
