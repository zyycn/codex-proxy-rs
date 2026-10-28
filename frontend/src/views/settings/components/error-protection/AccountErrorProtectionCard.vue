<script setup lang="ts">
import type { ProtectionDraft } from './form'
import type { ProtectionRule } from '@/api/modules/settings/protection'
import { ZButton, ZCard, ZCheckbox, ZForm, ZFormItem, ZIconButton, ZInput, ZPopover, ZSwitch } from '@codex-proxy/ui'

import { Activity, CircleAlert, Gauge, Plus, Snowflake, Timer } from '@lucide/vue'

import { computed, shallowRef, useId } from 'vue'
import { useI18n } from 'vue-i18n'
import { generateRequestId } from '@/utils/operation'
import { createProtectionCondition, protectionLimits } from './form'
import ProtectionRuleRow from './ProtectionRuleRow.vue'

const props = defineProps<{ disabled: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const policy = defineModel<ProtectionDraft>({ required: true })
const expandedId = shallowRef<string | null>(null)
const rulesError = computed(() => policy.value.enabled && !policy.value.rules.length
  ? t('settings.errorProtection.validation.required')
  : '')
const probeModelId = useId()
const helpId = useId()

function update(patch: Partial<ProtectionDraft>) {
  if (!props.disabled) {
    policy.value = {
      ...policy.value,
      ...patch,
    }
  }
}

function numericModel(key: 'threshold' | 'windowSeconds' | 'durationSeconds') {
  return computed({
    get: () => policy.value[key] === null
      ? ''
      : String(policy.value[key]),
    set: (value: string) => update({
      [key]: value.trim() && Number.isFinite(Number(value))
        ? Number(value)
        : null,
    }),
  })
}

const threshold = numericModel('threshold')
const windowSeconds = numericModel('windowSeconds')
const durationSeconds = numericModel('durationSeconds')

function addRule() {
  const rule: ProtectionRule = {
    id: generateRequestId(),
    name: '',
    conditions: [
      createProtectionCondition(),
    ],
  }
  update({
    rules: [...policy.value.rules, rule],
  })
  expandedId.value = rule.id
}

function updateRule(rule: ProtectionRule) {
  update({
    rules: policy.value.rules.map(previous => previous.id === rule.id
      ? rule
      : previous),
  })
}

function removeRule(id: string) {
  update({
    rules: policy.value.rules.filter(rule => rule.id !== id),
  })
  if (expandedId.value === id)
    expandedId.value = null
}
</script>

<template>
  <ZCard
    :title="t('settings.errorProtection.title')"
    :description="t('settings.errorProtection.description')"
  >
    <ZForm class="max-w-6xl sm:grid-cols-2 sm:items-end">
      <ZSwitch
        :model-value="policy.enabled"
        :disabled="disabled"
        class="col-span-full justify-self-start"
        :label="t('settings.errorProtection.enable')"
        @update:model-value="update({ enabled: $event })"
      />

      <section class="col-span-full grid min-w-0 gap-3" :aria-label="t('settings.errorProtection.rules')">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div class="flex min-w-0 items-center gap-1">
            <span class="text-cp-sm font-emphasis text-cp-text-secondary">{{ t('settings.errorProtection.anyRule') }}</span>
            <ZPopover trigger="hover-click" placement="top-start" :hover-delay="240">
              <template #reference="{ open }">
                <ZIconButton
                  size="small"
                  :aria-label="t('settings.errorProtection.matchingHelpTitle')"
                  :aria-expanded="open"
                  :aria-describedby="open ? helpId : undefined"
                >
                  <CircleAlert class="size-3.5" />
                </ZIconButton>
              </template>
              <div :id="helpId" class="grid max-w-80 gap-2 px-3 py-2 text-cp-sm leading-relaxed text-cp-text-secondary">
                <p class="m-0">
                  {{ t('settings.errorProtection.matchingHelp') }}
                </p>
                <p class="m-0">
                  {{ t('settings.errorProtection.missingHelp') }}
                </p>
                <p class="m-0">
                  {{ t('settings.errorProtection.limitsHelp') }}
                </p>
              </div>
            </ZPopover>
          </div>
          <ZButton size="small" :disabled="disabled || policy.rules.length >= protectionLimits.rules" @click="addRule">
            <template #icon>
              <Plus class="size-3.5" />
            </template>
            {{ t('settings.errorProtection.addRule') }}
          </ZButton>
        </div>
        <p v-if="!policy.rules.length" class="m-0 rounded-cp bg-cp-fill-quaternary px-4 py-4 text-cp-sm text-cp-text-tertiary">
          {{ t('settings.errorProtection.emptyRules') }}
        </p>
        <div v-else class="grid min-w-0 gap-2">
          <template v-for="(rule, index) in policy.rules" :key="rule.id">
            <span v-if="index > 0" class="pl-4 text-cp-xs font-emphasis text-cp-text-tertiary">{{ t('settings.errorProtection.or') }}</span>
            <ProtectionRuleRow
              :rule="rule"
              :expanded="expandedId === rule.id"
              :disabled="disabled"
              @toggle="expandedId = expandedId === rule.id ? null : rule.id"
              @remove="removeRule(rule.id)"
              @update="updateRule"
            />
          </template>
        </div>
        <p v-if="rulesError" class="m-0 text-cp-sm text-cp-error-text" role="alert">
          {{ rulesError }}
        </p>
      </section>

      <ZFormItem
        :label="t('settings.errorProtection.failureCountThreshold')"
        :description="t('settings.errorProtection.thresholdHelp')"
      >
        <ZInput
          v-model="threshold"
          :disabled="disabled || !policy.enabled"
          :aria-label="t('settings.errorProtection.failureCountThreshold')"
          type="number"
          min="2"
          max="1000"
          step="1"
        >
          <template #prefix>
            <Activity class="size-4" />
          </template>
          <template #suffix>
            <span class="text-cp-sm">{{ t('settings.errorProtection.times') }}</span>
          </template>
        </ZInput>
      </ZFormItem>

      <ZFormItem
        :label="t('settings.errorProtection.countValidity')"
        :description="t('settings.errorProtection.windowHelp')"
      >
        <ZInput
          v-model="windowSeconds"
          :disabled="disabled || !policy.enabled"
          :aria-label="t('settings.errorProtection.countValiditySeconds')"
          type="number"
          min="60"
          max="3600"
          step="1"
        >
          <template #prefix>
            <Timer class="size-4" />
          </template>
          <template #suffix>
            <span class="text-cp-sm">{{ t('settings.errorProtection.seconds') }}</span>
          </template>
        </ZInput>
      </ZFormItem>

      <ZFormItem
        :label="t('settings.errorProtection.cooldownTime')"
        :description="t('settings.errorProtection.cooldownHelp')"
      >
        <ZInput
          v-model="durationSeconds"
          :disabled="disabled || !policy.enabled"
          :aria-label="t('settings.errorProtection.cooldownTimeInSeconds')"
          type="number"
          min="300"
          max="604800"
          step="1"
        >
          <template #prefix>
            <Snowflake class="size-4" />
          </template>
          <template #suffix>
            <span class="text-cp-sm">{{ t('settings.errorProtection.seconds') }}</span>
          </template>
        </ZInput>
      </ZFormItem>
      <ZFormItem
        :label="t('settings.errorProtection.recoveryProbeModel')"
        :description="t('settings.errorProtection.probeModelHelp')"
        :for="probeModelId"
      >
        <template #extra>
          <ZCheckbox
            :model-value="policy.probeEnabled"
            :disabled="disabled || !policy.enabled"
            :label="t('settings.errorProtection.requireSuccessfulProbe')"
            @update:model-value="update({ probeEnabled: $event })"
          />
        </template>
        <ZInput
          :id="probeModelId"
          :model-value="policy.probeModel"
          :disabled="disabled || !policy.enabled || !policy.probeEnabled"
          :aria-label="t('settings.errorProtection.recoveryProbeModel')"
          :placeholder="t('settings.errorProtection.modelPlaceholder')"
          @update:model-value="update({ probeModel: $event })"
        >
          <template #prefix>
            <Gauge class="size-4" />
          </template>
        </ZInput>
      </ZFormItem>
    </ZForm>
  </ZCard>
</template>
