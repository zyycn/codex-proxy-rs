<script setup lang="ts">
import { ZButton, ZConfirmDialog, ZForm, ZIconButton, ZPageHeader, ZSegmented } from '@codex-proxy/ui'

import { Save, Undo2 } from '@lucide/vue'

import { computed, nextTick, reactive, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'

import AccountAffinityCard from './components/AccountAffinityCard.vue'
import AccountWarmupCard from './components/AccountWarmupCard.vue'
import SettingsBackupSection from './components/backup/SettingsBackupSection.vue'
import ClientProfileCard from './components/ClientProfileCard.vue'
import ConcurrencyPolicyCard from './components/ConcurrencyPolicyCard.vue'
import AccountErrorProtectionCard from './components/error-protection/AccountErrorProtectionCard.vue'
import ModelAliasesCard from './components/ModelAliasesCard.vue'
import PrivacyPolicyCard from './components/privacy/PrivacyPolicyCard.vue'
import RequestHistoryCard from './components/RequestHistoryCard.vue'
import RequestLocationCard from './components/RequestLocationCard.vue'
import ResetCreditCard from './components/ResetCreditCard.vue'
import RotationStrategyCard from './components/RotationStrategyCard.vue'
import SettingsAccessSection from './components/SettingsAccessSection.vue'
import TokenRefreshCard from './components/TokenRefreshCard.vue'
import { useSettingsForm } from './composables/useSettingsForm'
import { useSettingsFormRules } from './composables/useSettingsFormRules'
import { useSettingsLeaveGuard } from './composables/useSettingsLeaveGuard'
import { rotationOptions } from './constants'
import PricingSection from './pricing/index.vue'

const { t } = useI18n({ useScope: 'global' })

const route = useRoute()
const router = useRouter()
const sectionOptions = computed(() => ([
  {
    label: t('settings.page.tabs.runtime'),
    value: 'runtime',
  },
  {
    label: t('settings.page.tabs.upstream'),
    value: 'upstream',
  },
  {
    label: t('settings.page.tabs.pricing'),
    value: 'pricing',
  },
  {
    label: t('settings.page.tabs.access'),
    value: 'access',
  },
  {
    label: t('settings.page.tabs.backups'),
    value: 'backup',
  },
]))
const section = computed(() => {
  switch (route.name) {
    case 'settings-upstream': return 'upstream'
    case 'settings-access': return 'access'
    case 'settings-pricing': return 'pricing'
    case 'settings-backup': return 'backup'
    default: return 'runtime'
  }
})
const isBasicSection = computed(() => section.value !== 'pricing' && section.value !== 'backup')
const visited = reactive(new Set<string>())
const settingsVisited = shallowRef(false)

function switchSection(value = 'runtime') {
  return router.push(value === 'runtime'
    ? '/settings'
    : `/settings/${value}`)
}

const {
  loading,
  saving,
  detectingLocation,
  detectLocation,
  hasChanges,
  resetSettings,
  error,
  form,
  smartSchedulingDefaults,
  mappings,
  addMapping,
  updateMapping,
  removeMapping,
  refreshMarginSecondsValue,
  refreshConcurrencyValue,
  maxConcurrentPerAccountValue,
  openaiGuardianReservedConcurrencyValue,
  maxAccountRotationsValue,
  openaiSessionAffinityTtlHoursValue,
  maxWaitingPerKeyValue,
  maxWaitingPerAccountValue,
  keyQueueTimeoutSecondsValue,
  accountQueueTimeoutSecondsValue,
  maxRetryAfterSecondsValue,
  responsesMaxDecompressedBodyMiBValue,
  usageRetentionDaysValue,
  resetCreditLeadMinutesValue,
  resetCreditMaxWeeklyRemainingPercentValue,
  saveSettings,
  loadSettings,
} = useSettingsForm()
const disabled = computed(() => saving.value || loading.value || detectingLocation.value || !!error.value)
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const formModel = computed(() => ({ ...form, modelMappings: mappings.value }))
const rules = useSettingsFormRules(() => form)

async function requestSave() {
  if (disabled.value || !hasChanges.value)
    return
  await formRef.value?.validate(async (valid, errors) => {
    if (valid) {
      await saveSettings()
      return
    }
    const path = Object.keys(errors ?? {})[0]
    if (!path)
      return
    const field = formRef.value?.getField(path)
    const target = field?.$el?.closest('[data-settings-section]')?.getAttribute('data-settings-section')
    if (target)
      await switchSection(target)
    await nextTick()
    field?.scrollIntoView({ block: 'center' })
    field?.focus()
  })
}

function resetForm() {
  resetSettings()
  formRef.value?.clearValidate()
}
const backupSection = useTemplateRef<InstanceType<typeof SettingsBackupSection>>('backupSection')
const hasUnsavedChanges = computed(() => hasChanges.value || !!backupSection.value?.hasChanges)
const { showLeaveConfirm, confirmLeave } = useSettingsLeaveGuard(hasUnsavedChanges)

watch(() => !loading.value && route.hash === '#reset-credits', async (ready) => {
  if (ready) {
    await nextTick()
    document.getElementById('reset-credits')?.scrollIntoView({ block: 'start' })
  }
}, { immediate: true })

watch(section, (value) => {
  visited.add(value)
  if (isBasicSection.value && !settingsVisited.value) {
    settingsVisited.value = true
    void loadSettings()
  }
}, { immediate: true })
</script>

<template>
  <div class="w-full" :class="section === 'pricing' ? 'flex h-[calc(100dvh-2rem)] flex-none! flex-col min-[961px]:h-[calc(100dvh-3rem)]' : undefined">
    <ZPageHeader :title="t('navigation.settings')" :description="t('settings.page.description')" />

    <div class="mt-4 flex min-h-z-control shrink-0 flex-wrap items-center justify-between gap-3">
      <ZSegmented
        :model-value="section"
        :aria-label="t('settings.page.sectionsLabel')"
        class="w-full sm:w-auto"
        :options="sectionOptions"
        @update:model-value="switchSection"
      />
      <div v-if="isBasicSection || hasChanges" class="ml-auto flex items-center justify-end gap-2">
        <span v-if="hasChanges" class="mr-1 size-1.5 shrink-0 rounded-full bg-z-warning" aria-hidden="true" />
        <ZIconButton v-if="hasChanges" :aria-label="t('settings.page.undoAllBasicSettingsChanges')" variant="solid" :disabled="saving || loading || detectingLocation" @click="resetForm">
          <Undo2 class="size-4" />
        </ZIconButton>
        <ZButton type="primary" :loading="saving" :disabled="disabled || !hasChanges" @click="requestSave">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? t('settings.backup.schedule.saving') : t('settings.page.saveChanges') }}
        </ZButton>
      </div>
    </div>

    <ZForm v-if="settingsVisited" v-show="isBasicSection" ref="formElement" :model="formModel" :rules="rules" :validate-on-rule-change="false" novalidate class="mt-5 w-full" @submit="requestSave">
      <SettingsAccessSection
        v-if="visited.has('access')"
        v-show="section === 'access'"
        v-model:min-codex-desktop-version="form.minCodexDesktopVersion"
        v-model:min-codex-cli-version="form.minCodexCliVersion"
        v-model:responses-max-decompressed-body-mi-b="responsesMaxDecompressedBodyMiBValue"
        data-settings-section="access"
        :disabled="disabled"
        :loading="loading"
      />

      <fieldset v-show="section !== 'access'" :disabled="disabled" class="m-0 grid min-w-0 gap-5 border-0 p-0" :aria-label="t('settings.page.basicSettings')">
        <div v-if="visited.has('runtime')" v-show="section === 'runtime'" data-settings-section="runtime" class="grid min-w-0 gap-5">
          <RotationStrategyCard
            v-model="form.rotationStrategy"
            v-model:smart-scheduling="form.smartScheduling"
            :smart-defaults="smartSchedulingDefaults"
            :disabled="disabled"
            :options="rotationOptions"
          />
          <AccountAffinityCard
            v-model="form.openaiAccountAffinity"
            v-model:ttl-hours="openaiSessionAffinityTtlHoursValue"
            v-model:max-account-rotations="maxAccountRotationsValue"
            :disabled="disabled"
          />
          <ConcurrencyPolicyCard
            v-model:max-concurrent-per-account="maxConcurrentPerAccountValue"
            v-model:max-waiting-per-key="maxWaitingPerKeyValue"
            v-model:max-waiting-per-account="maxWaitingPerAccountValue"
            v-model:key-queue-timeout-seconds="keyQueueTimeoutSecondsValue"
            v-model:account-queue-timeout-seconds="accountQueueTimeoutSecondsValue"
            v-model:switch-account-on-queue-failure="form.switchAccountOnQueueFailure"
            v-model:max-retry-after-seconds="maxRetryAfterSecondsValue"
            v-model:openai-guardian-reserved-concurrency="openaiGuardianReservedConcurrencyValue"
          />
          <AccountErrorProtectionCard v-model="form.accountErrorProtection" :disabled="disabled" />
          <AccountWarmupCard
            v-model:enabled="form.accountWarmupEnabled"
            v-model:cron="form.accountWarmupCron"
            v-model:model="form.accountWarmupModel"
          />
          <ResetCreditCard
            v-model="resetCreditLeadMinutesValue"
            v-model:max-weekly-remaining-percent="resetCreditMaxWeeklyRemainingPercentValue"
          />
          <RequestHistoryCard v-model="usageRetentionDaysValue" />
        </div>

        <div v-if="visited.has('upstream')" v-show="section === 'upstream'" data-settings-section="upstream" class="grid min-w-0 gap-5">
          <TokenRefreshCard v-model:refresh-margin-seconds="refreshMarginSecondsValue" v-model:refresh-concurrency="refreshConcurrencyValue" />
          <ClientProfileCard
            v-model="form.providerRequestProfiles"
            :active="section === 'upstream'"
            :disabled="disabled"
          />
          <RequestLocationCard
            v-model="form.requestLocation"
            v-model:enabled="form.requestLocationEnabled"
            :disabled="disabled"
            :detecting-location="detectingLocation"
            @detect-location="detectLocation"
          />
          <PrivacyPolicyCard v-model="form.codexPrivacyPolicy" :disabled="disabled" />
          <ModelAliasesCard
            :mappings="mappings"
            :loading="loading"
            @add-mapping="addMapping"
            @update-mapping="updateMapping"
            @remove-mapping="removeMapping"
          />
        </div>
      </fieldset>
    </ZForm>

    <SettingsBackupSection v-if="visited.has('backup')" v-show="section === 'backup'" ref="backupSection" class="mt-5" :active="section === 'backup'" />
    <PricingSection v-if="visited.has('pricing')" v-show="section === 'pricing'" class="mt-5 min-h-0 flex-1" />
    <ZConfirmDialog
      v-model="showLeaveConfirm"
      :title="t('settings.page.unsavedChanges.title')"
      :description="t('settings.page.unsavedChanges.description')"
      :confirm-text="t('settings.page.unsavedChanges.leave')"
      :cancel-text="t('settings.page.unsavedChanges.stay')"
      @confirm="confirmLeave()"
    />
  </div>
</template>
