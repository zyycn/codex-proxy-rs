<script setup lang="ts">
import { ZCard, ZFormItem, ZIconButton, ZInput, ZPopover, ZSwitch } from '@codex-proxy/ui'

import { CircleAlert, Clock, Sparkles } from '@lucide/vue'
import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const enabled = defineModel<boolean>('enabled', { required: true })
const scheduleTime = defineModel<string>('scheduleTime', { required: true })
const model = defineModel<string>('model', { required: true })
</script>

<template>
  <ZCard
    :description="t('settings.warmup.description')"
  >
    <template #title>
      <span class="inline-flex items-center gap-1">
        {{ t('settings.warmup.title') }}
        <ZPopover trigger="hover-click" placement="top-start">
          <template #reference="{ open }">
            <ZIconButton variant="link" :aria-label="t('settings.warmup.helpTitle')" :aria-expanded="open" class="size-6!">
              <CircleAlert class="size-3.5" aria-hidden="true" />
            </ZIconButton>
          </template>
          <p class="max-w-72 px-3 py-2 text-z-sm font-regular leading-relaxed text-z-text-secondary">
            {{ t('settings.warmup.help') }}
          </p>
        </ZPopover>
      </span>
    </template>
    <div class="grid min-w-0 gap-4.5 max-w-6xl sm:grid-cols-2 sm:items-end">
      <ZSwitch
        v-model="enabled"
        class="col-span-full justify-self-start"
        :label="t('settings.warmup.enableDailyWarmup')"
      />

      <ZFormItem
        :show-message="Boolean(scheduleTime.trim())"
        prop="accountWarmupScheduleTime"
        :label="t('settings.warmup.executionTime')"
        :description="t('settings.warmup.timesPlaceholder')"
      >
        <ZInput
          v-model="scheduleTime"
          :disabled="!enabled"
          :aria-label="t('settings.warmup.scheduleLabel')"
          placeholder="08:00"
        >
          <template #prefix>
            <Clock class="size-4" />
          </template>
        </ZInput>
      </ZFormItem>

      <ZFormItem
        :show-message="Boolean(model.trim())"
        prop="accountWarmupModel"
        :label="t('settings.warmup.modelLabel')"
        :description="t('settings.warmup.modelHelp')"
        :required="enabled"
      >
        <ZInput
          v-model="model"
          :disabled="!enabled"
          :aria-label="t('settings.warmup.modelLabel')"
          :placeholder="t('settings.warmup.modelPlaceholder')"
        >
          <template #prefix>
            <Sparkles class="size-4" />
          </template>
        </ZInput>
      </ZFormItem>
    </div>
  </ZCard>
</template>
