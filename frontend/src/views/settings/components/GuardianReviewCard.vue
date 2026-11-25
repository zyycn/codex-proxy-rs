<script setup lang="ts">
import { ZCard, ZFormItem, ZIconButton, ZInput, ZPopover, ZSwitch } from '@codex-proxy/ui'
import { CircleAlert } from '@lucide/vue'
import { useI18n } from 'vue-i18n'
import HelpPopover from '@/components/HelpPopover.vue'

defineProps<{ disabled: boolean }>()
const { t } = useI18n({ useScope: 'global' })
const enabled = defineModel<boolean>({ required: true })
const reservedConcurrency = defineModel<string>('reservedConcurrency', { required: true })
</script>

<template>
  <ZCard id="oauth-guardian-review-model-help" :title="t('settings.routing.guardianReviewTitle')" :description="t('settings.routing.guardianReviewDescription')" class="@container">
    <div class="grid min-w-0 gap-4.5 @md:grid-cols-2 @md:items-start">
      <ZFormItem :show-message="false" :label="t('settings.routing.oauthGuardianReviewModel')" :description="t('settings.routing.oauthGuardianReviewModelHelp')">
        <template #label-extra>
          <HelpPopover
            :label="t('common.form.fieldHelp', { label: t('settings.routing.oauthGuardianReviewModel') })"
            :content="t('settings.routing.oauthGuardianReviewModelDetails')"
          />
        </template>
        <ZSwitch
          v-model="enabled"
          class="@md:min-h-z-control"
          :aria-label="t('settings.routing.oauthGuardianReviewModel')"
          :aria-description="t('settings.routing.oauthGuardianReviewModelHelp')"
          :active-text="t('common.values.on')"
          :inactive-text="t('common.values.off')"
          inline-prompt
          :width="56"
          :disabled="disabled"
        />
      </ZFormItem>
      <ZFormItem :show-message="Boolean(reservedConcurrency.trim())" prop="openaiGuardianReservedConcurrency" :label="t('settings.concurrency.autoReview.label')" :description="t('settings.concurrency.autoReview.description')">
        <template #label-extra>
          <ZPopover class="-my-1" trigger="hover-click" placement="top-start">
            <template #reference="{ open }">
              <ZIconButton variant="link" :aria-label="t('settings.concurrency.autoReview.helpTitle')" :aria-expanded="open" class="size-6!">
                <CircleAlert class="size-3.5" aria-hidden="true" />
              </ZIconButton>
            </template>
            <div class="max-w-72 space-y-2 px-3 py-2 text-z-sm leading-relaxed text-z-text-secondary">
              <p>{{ t('settings.scheduling.autoReviewSlotsHelp') }}</p>
              <p>{{ t('settings.scheduling.autoReviewQueueHelp') }}</p>
            </div>
          </ZPopover>
        </template>
        <ZInput v-model="reservedConcurrency" :aria-label="t('settings.concurrency.autoReview.label')" :disabled="disabled" type="number" min="0" max="4294967295" step="1" />
      </ZFormItem>
    </div>
  </ZCard>
</template>
