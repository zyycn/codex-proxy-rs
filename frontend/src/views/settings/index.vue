<script setup lang="ts">
import type { BuiltinProvider } from '@/providers/catalog'
import { ZButton, ZConfirmDialog, ZForm, ZIconButton, ZPageHeader, ZSegmented } from '@codex-proxy/ui'

import { Save, Undo2 } from '@lucide/vue'

import { computed, nextTick, reactive, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import ProviderSelect from '@/components/ProviderSelect.vue'

import { isBuiltinProvider, PROVIDER_IDS } from '@/providers/catalog'

import AccountAffinityCard from './components/AccountAffinityCard.vue'
import AccountWarmupCard from './components/AccountWarmupCard.vue'
import SettingsBackupSection from './components/backup/index.vue'
import ClientVersionSettings from './components/client-version/index.vue'
import ConcurrencyPolicyCard from './components/ConcurrencyPolicyCard.vue'
import AccountErrorProtectionCard from './components/error-protection/index.vue'
import GuardianReviewCard from './components/GuardianReviewCard.vue'
import ModelAliasesCard from './components/ModelAliasesCard.vue'
import OpenAiRequestCard from './components/OpenAiRequestCard.vue'
import PricingSection from './components/pricing/index.vue'
import PrivacyPolicyCard from './components/privacy/index.vue'
import PromptRewriteRulesCard from './components/PromptRewriteRulesCard.vue'
import RequestHistoryCard from './components/RequestHistoryCard.vue'
import RequestLocationCard from './components/RequestLocationCard.vue'
import RequestProfileCard from './components/RequestProfileCard.vue'
import ResetCreditCard from './components/ResetCreditCard.vue'
import ResponseBodyLimitCard from './components/ResponseBodyLimitCard.vue'
import RetryPolicyCard from './components/RetryPolicyCard.vue'
import RotationStrategyCard from './components/RotationStrategyCard.vue'
import SettingsAccessSection from './components/settings-access/index.vue'
import TokenRefreshCard from './components/TokenRefreshCard.vue'
import WebSocketPoolCard from './components/WebSocketPoolCard.vue'
import { useSettingsForm } from './composables/useSettingsForm'
import { useSettingsFormRules } from './composables/useSettingsFormRules'
import { useSettingsLeaveGuard } from './composables/useSettingsLeaveGuard'
import { rotationOptions } from './constants'

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
    label: t('settings.page.tabs.backups'),
    value: 'backup',
  },
]))
const section = computed(() => {
  switch (route.name) {
    case 'settings-upstream': return 'upstream'
    case 'settings-pricing': return 'pricing'
    case 'settings-backup': return 'backup'
    default: return 'runtime'
  }
})
const isBasicSection = computed(() => section.value !== 'pricing' && section.value !== 'backup')
const visited = reactive(new Set<string>())
const settingsVisited = shallowRef(false)
const provider = shallowRef<BuiltinProvider>(PROVIDER_IDS[0]!)

function switchSection(value = 'runtime', selectedProvider = provider.value) {
  return router.push({
    path: value === 'runtime'
      ? '/settings'
      : `/settings/${value}`,
    query: value === 'upstream'
      ? { provider: selectedProvider }
      : undefined,
  })
}

function switchProvider(value: BuiltinProvider) {
  return switchSection('upstream', value)
}

watch(() => route.query.provider, (value) => {
  if (typeof value === 'string' && isBuiltinProvider(value))
    provider.value = value
}, { immediate: true })

const {
  loading,
  saving,
  detectingLocation,
  hasChanges,
  resetSettings,
  error,
  form,
  mappings,
  responsesMaxDecompressedBodyMiBValue,
  saveSettings,
  loadSettings,
  smartSchedulingDefaults,
  quotaResetWindowWeightsDefaults,
  maxConcurrentPerAccountValue,
  maxAccountRotationsValue,
  sessionAffinityTtlHoursValue,
  maxWaitingPerKeyValue,
  maxWaitingPerAccountValue,
  keyQueueTimeoutSecondsValue,
  accountQueueTimeoutSecondsValue,
  maxRetryAfterSecondsValue,
  usageRetentionDaysValue,
  resetCreditLeadMinutesValue,
  resetCreditMaxWeeklyRemainingPercentValue,
  detectLocation,
  addMapping,
  updateMapping,
  removeMapping,
  refreshMarginSecondsValue,
  refreshConcurrencyValue,
  openaiGuardianReservedConcurrencyValue,
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
    const targetProvider = field?.$el?.closest('[data-settings-provider]')?.getAttribute('data-settings-provider')
    if (target) {
      await switchSection(target, targetProvider && isBuiltinProvider(targetProvider)
        ? targetProvider
        : provider.value)
    }
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
const { showLeaveConfirm, confirmLeave, finishLeave } = useSettingsLeaveGuard(hasUnsavedChanges)

watch(section, (value) => {
  visited.add(value)
  if (isBasicSection.value && !settingsVisited.value) {
    settingsVisited.value = true
    void loadSettings()
  }
}, { immediate: true })

watch(() => !loading.value && route.hash === '#reset-credits', async (ready) => {
  if (!ready)
    return
  if (section.value !== 'upstream' || provider.value !== 'openai') {
    await router.replace({
      path: '/settings/upstream',
      query: { provider: 'openai' },
      hash: '#reset-credits',
    })
  }
  await nextTick()
  document.getElementById('reset-credits')?.scrollIntoView({ block: 'start' })
}, { immediate: true })
</script>

<template>
  <div class="@container/settings w-full" :class="section === 'pricing' ? 'flex h-[calc(100dvh-2rem)] flex-none! flex-col min-[961px]:h-[calc(100dvh-3rem)]' : undefined">
    <ZPageHeader :title="t('navigation.settings')" :description="t('settings.page.description')" />

    <div class="mt-4 flex min-h-z-control shrink-0 flex-wrap items-center justify-between gap-3">
      <ZSegmented
        :model-value="section"
        :aria-label="t('settings.page.sectionsLabel')"
        class="w-full @lg/settings:w-auto @max-sm/settings:h-auto! @max-sm/settings:grid-cols-4! @max-sm/settings:[&>button]:px-1 @max-sm/settings:[&>button]:py-1 @max-sm/settings:[&>button>span]:leading-tight @max-sm/settings:[&>button>span]:whitespace-normal"
        :options="sectionOptions"
        @update:model-value="switchSection"
      />
      <ProviderSelect v-if="section === 'upstream'" :model-value="provider" display="label" class="w-full @sm/settings:w-auto @max-sm/settings:[&>button]:px-2" :aria-label="t('settings.page.providerLabel')" :disabled="disabled" @update:model-value="switchProvider" />
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

    <ZForm v-if="settingsVisited" v-show="isBasicSection" ref="formElement" :model="formModel" :rules="rules" :validate-on-rule-change="false" novalidate class="mt-5 w-full gap-5" @submit="requestSave">
      <fieldset :disabled="disabled" class="m-0 grid min-w-0 gap-5 border-0 p-0" :aria-label="t('settings.page.basicSettings')">
        <div
          v-if="visited.has('runtime')"
          v-show="section === 'runtime'"
          data-settings-section="runtime"
          class="grid min-w-0 gap-5"
        >
          <RotationStrategyCard
            v-model="form.rotationStrategy"
            v-model:smart-scheduling="form.smartScheduling"
            v-model:quota-reset-window-weights="form.quotaResetWindowWeights"
            :smart-defaults="smartSchedulingDefaults"
            :quota-reset-defaults="quotaResetWindowWeightsDefaults"
            :disabled="disabled"
            :options="rotationOptions"
          />
          <div class="grid min-w-0 gap-5 @6xl/settings:grid-cols-2">
            <ConcurrencyPolicyCard
              v-model:max-concurrent-per-account="maxConcurrentPerAccountValue"
              v-model:max-waiting-per-key="maxWaitingPerKeyValue"
              v-model:max-waiting-per-account="maxWaitingPerAccountValue"
              v-model:key-queue-timeout-seconds="keyQueueTimeoutSecondsValue"
              v-model:account-queue-timeout-seconds="accountQueueTimeoutSecondsValue"
              class="col-span-full"
            />
            <RetryPolicyCard
              v-model:switch-account-on-queue-failure="form.switchAccountOnQueueFailure"
              v-model:max-retry-after-seconds="maxRetryAfterSecondsValue"
              v-model:max-account-rotations="maxAccountRotationsValue"
              class="col-span-full"
            />
            <AccountAffinityCard
              v-model="form.accountAffinity"
              v-model:ttl-hours="sessionAffinityTtlHoursValue"
              :disabled="disabled"
            />
            <TokenRefreshCard v-model:refresh-margin-seconds="refreshMarginSecondsValue" v-model:refresh-concurrency="refreshConcurrencyValue" />
            <AccountErrorProtectionCard v-model="form.accountErrorProtection" class="col-span-full" :disabled="disabled" />
            <PromptRewriteRulesCard v-model="form.promptRewriteRules" :ready="!loading && !error" :disabled="disabled" class="col-span-full" />
            <ModelAliasesCard
              :mappings="mappings"
              :loading="loading"
              class="col-span-full"
              @add-mapping="addMapping"
              @update-mapping="updateMapping"
              @remove-mapping="removeMapping"
            />
            <RequestHistoryCard v-model="usageRetentionDaysValue" />
            <ResponseBodyLimitCard v-model="responsesMaxDecompressedBodyMiBValue" />
          </div>
        </div>

        <div
          v-if="visited.has('upstream')"
          v-show="section === 'upstream'"
          data-settings-section="upstream"
          class="grid min-w-0 gap-5"
        >
          <template v-for="providerId in PROVIDER_IDS" :key="providerId">
            <div
              v-show="provider === providerId"
              :data-settings-provider="providerId"
              class="grid min-w-0 gap-5 @6xl/settings:grid-cols-2"
            >
              <RequestProfileCard
                v-model="form.providerRequestProfiles"
                :provider="providerId"
                :active="section === 'upstream' && provider === providerId"
                :disabled="disabled"
                class="col-span-full"
              />
              <template v-if="providerId === 'openai'">
                <RequestLocationCard
                  v-model="form.requestLocation"
                  v-model:enabled="form.requestLocationEnabled"
                  :disabled="disabled"
                  :detecting-location="detectingLocation"
                  @detect-location="detectLocation"
                />
                <OpenAiRequestCard
                  v-model:request-country="form.openaiRequestSettings.requestCountry"
                  v-model:residency="form.openaiRequestSettings.residency"
                  v-model:account-routing-override="form.openaiRequestSettings.accountRoutingOverride"
                  :disabled="disabled"
                />
                <WebSocketPoolCard v-model="form.openaiWebsocketPool" :disabled="disabled" class="col-span-full" />
                <GuardianReviewCard
                  v-model="form.openaiRequestSettings.oauthGuardianReviewModel"
                  v-model:reserved-concurrency="openaiGuardianReservedConcurrencyValue"
                  :disabled="disabled"
                />
                <ResetCreditCard
                  v-model="resetCreditLeadMinutesValue"
                  v-model:on-low-weekly-quota="form.resetCreditOnLowWeeklyQuota"
                  v-model:max-weekly-remaining-percent="resetCreditMaxWeeklyRemainingPercentValue"
                />
                <AccountWarmupCard
                  v-model:enabled="form.accountWarmupEnabled"
                  v-model:cron="form.accountWarmupCron"
                  v-model:model="form.accountWarmupModel"
                />
                <ClientVersionSettings
                  v-model:min-codex-desktop-version="form.minCodexDesktopVersion"
                  v-model:min-codex-cli-version="form.minCodexCliVersion"
                  :loading="loading"
                />
                <div class="col-span-full min-w-0">
                  <PrivacyPolicyCard v-model="form.codexPrivacyPolicy" :disabled="disabled" />
                </div>
              </template>
            </div>
          </template>
        </div>
      </fieldset>
      <SettingsAccessSection
        v-if="visited.has('runtime')"
        v-show="section === 'runtime'"
        data-settings-section="runtime"
      />
    </ZForm>

    <SettingsBackupSection v-if="visited.has('backup')" v-show="section === 'backup'" ref="backupSection" class="mt-5" :active="section === 'backup'" />
    <PricingSection v-if="visited.has('pricing')" v-show="section === 'pricing'" class="mt-5 min-h-0 flex-1" />
    <ZConfirmDialog
      v-model="showLeaveConfirm"
      :title="t('settings.page.unsavedChanges.title')"
      :description="t('settings.page.unsavedChanges.description')"
      :confirm-text="t('settings.page.unsavedChanges.leave')"
      :cancel-text="t('settings.page.unsavedChanges.stay')"
      @confirm="confirmLeave"
      @closed="finishLeave"
    />
  </div>
</template>
