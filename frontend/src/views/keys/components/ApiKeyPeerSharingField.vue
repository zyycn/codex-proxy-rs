<script setup lang="ts">
import type { ApiKeyPeerSharing } from '@/api'
import { ZCheckbox, ZFormItem, ZSwitch } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import HelpPopover from '@/components/HelpPopover.vue'

defineProps<{ sharing: Partial<ApiKeyPeerSharing>, disabled: boolean }>()
const emit = defineEmits<{ change: [field: keyof ApiKeyPeerSharing, value: boolean] }>()
const { t } = useI18n({ useScope: 'global' })
const permissions = computed(() => ({
  sharePool: t('keys.editor.sharePool'),
  shareQuota: t('keys.editor.shareQuota'),
  sharePlans: t('keys.editor.sharePlans'),
}))
</script>

<template>
  <ZFormItem :label="t('keys.editor.peerSharing')">
    <template #label-extra>
      <HelpPopover :label="t('keys.editor.peerSharing')" :content="t('keys.editor.peerSharingHelp')" />
    </template>
    <div class="flex flex-wrap items-center gap-x-5 gap-y-3">
      <ZSwitch
        :model-value="sharing.enabled === true" :aria-label="t('keys.editor.peerSharing')" :disabled="disabled"
        @update:model-value="emit('change', 'enabled', $event)"
      />
      <span v-if="sharing.enabled === undefined" class="text-z-sm text-z-text-secondary">{{ t('keys.batch.partiallyEnabled') }}</span>
      <template v-if="sharing.enabled">
        <ZCheckbox
          v-for="(label, field) in permissions" :key="field"
          :model-value="sharing[field] === true" :indeterminate="sharing[field] === undefined"
          :label="label" :disabled="disabled || (field === 'sharePlans' && !sharing.sharePool)"
          @update:model-value="emit('change', field, $event)"
        />
      </template>
    </div>
  </ZFormItem>
</template>
