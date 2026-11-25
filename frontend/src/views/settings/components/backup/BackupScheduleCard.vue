<script setup lang="ts">
import { ZButton, ZCard, ZForm, ZFormItem, ZInput, ZSwitch } from '@codex-proxy/ui'

import { CalendarClock, Save } from '@lucide/vue'

import { useI18n } from 'vue-i18n'

defineProps<{
  disabled: boolean
  saving: boolean
  storageReady: boolean
}>()

const emit = defineEmits<{
  save: []
}>()

const { t } = useI18n({ useScope: 'global' })

interface ScheduleForm {
  scheduleEnabled: boolean
  cronExpression: string
  retentionDays: string
  retentionCount: string
}

const schedule = defineModel<ScheduleForm>('schedule', { required: true })

function updateSchedule<Key extends keyof ScheduleForm>(key: Key, value: ScheduleForm[Key]) {
  schedule.value = { ...schedule.value, [key]: value }
}
</script>

<template>
  <ZCard
    :title="t('settings.backup.schedule.backupPlan')"
    :description="t('settings.backup.schedule.description')"
  >
    <template #actions>
      <div class="flex flex-wrap items-center gap-3">
        <ZSwitch
          :model-value="schedule.scheduleEnabled"
          :disabled="disabled || (!schedule.scheduleEnabled && !storageReady)"
          :aria-label="t('settings.backup.schedule.enableScheduledBackup')"
          :active-text="t('common.values.on')"
          :inactive-text="t('common.values.off')"
          inline-prompt
          :width="56"
          @update:model-value="updateSchedule('scheduleEnabled', $event)"
        />
        <ZButton type="primary" :loading="saving" :disabled="disabled || (schedule.scheduleEnabled && !storageReady)" @click="emit('save')">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? t('settings.backup.schedule.saving') : t('common.actions.save') }}
        </ZButton>
      </div>
    </template>

    <div class="@container">
      <ZForm class="@md:grid-cols-2 @md:items-end @5xl:grid-cols-3">
        <ZFormItem
          class="@md:col-span-full @5xl:col-span-1"
          :label="t('settings.backup.schedule.cronExpression')"
          :description="t('settings.backup.schedule.cronHelp')"
        >
          <ZInput :model-value="schedule.cronExpression" :disabled="disabled" :aria-label="t('settings.backup.schedule.cronExpression')" @update:model-value="updateSchedule('cronExpression', $event)">
            <template #prefix>
              <CalendarClock class="size-4" />
            </template>
          </ZInput>
        </ZFormItem>

        <ZFormItem :label="t('settings.backup.schedule.retentionDaysLabel')" :description="t('settings.backup.schedule.retentionDaysHelp')">
          <ZInput :model-value="schedule.retentionDays" :disabled="disabled" :aria-label="t('settings.backup.schedule.retentionDaysLabel')" type="number" min="0" @update:model-value="updateSchedule('retentionDays', $event)" />
        </ZFormItem>

        <ZFormItem
          :label="t('settings.backup.schedule.maxBackupsLabel')"
          :description="t('settings.backup.schedule.retentionCountHelp')"
        >
          <ZInput :model-value="schedule.retentionCount" :disabled="disabled" :aria-label="t('settings.backup.schedule.maxBackupsLabel')" type="number" min="0" @update:model-value="updateSchedule('retentionCount', $event)" />
        </ZFormItem>
      </ZForm>
    </div>
  </ZCard>
</template>
