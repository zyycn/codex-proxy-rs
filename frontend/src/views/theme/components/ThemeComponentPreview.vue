<script setup lang="ts">
import type { PaginationState } from '@codex-proxy/ui'

import { defineTableColumns, ZFormItem as FormItem, ZButton, ZCard, ZCheckbox, ZColorPicker, ZEmpty, ZForm, ZIconButton, ZInput, ZInputNumber, ZMenuItem, ZMotionIcon, ZPagination, ZPopover, ZRadio, ZSegmented, ZSelect, ZSkeleton, ZSlider, ZSwitch, ZTable, ZTag, ZTextarea } from '@codex-proxy/ui'

import {
  Bell,
  Check,
  CircleAlert,
  Download,
  Info,
  KeyRound,
  MoreHorizontal,
  Plus,
  Search,
  Trash2,
  TriangleAlert,
} from '@lucide/vue'

import { computed, shallowRef } from 'vue'

import { useI18n } from 'vue-i18n'
import { formatProviderLabel } from '@/utils/providers'

const { t } = useI18n({ useScope: 'global' })

interface PreviewTableRow {
  id: string
  account: string
  provider: string
  status: 'active' | 'rate_limited' | 'disabled' | 'error'
  requests: string
  latency: string
  updatedAt: string
}

// 示例文案随语言变化，用户编辑后保留输入内容
const search = shallowRef<string>()
const notes = shallowRef<string>()
const brandColor = shallowRef('#5983F4')
const invalidKey = shallowRef('sk-invalid')
const provider = shallowRef('openai')
const view = shallowRef('all')
const notifications = shallowRef(true)
const autoRotate = shallowRef(false)
const selected = shallowRef(true)
const partialSelected = shallowRef(false)
const strategy = shallowRef('balanced')
const controlHeight = shallowRef(38)
const syncTagVisible = shallowRef(true)

const providerOptions = computed(() => ([
  {
    label: formatProviderLabel('openai'),
    value: 'openai',
  },
  {
    label: formatProviderLabel('xai'),
    value: 'xai',
  },
  {
    label: t('appearance.preview.components.allPlatforms'),
    value: 'all',
  },
]))
const viewOptions = computed(() => ([
  {
    label: t('common.values.all'),
    value: 'all',
  },
  {
    label: t('common.status.healthy'),
    value: 'active',
  },
  {
    label: t('appearance.preview.components.restricted'),
    value: 'limited',
  },
]))
const themeSwatches = [
  { label: 'Container', className: 'bg-cp-bg-container' },
  { label: 'Fill Alter', className: 'bg-cp-fill-alter' },
  { label: 'Elevated', className: 'bg-cp-bg-elevated' },
  { label: 'Accent', className: 'bg-cp-primary' },
  { label: 'Success', className: 'bg-cp-success' },
  { label: 'Warning', className: 'bg-cp-warning' },
  { label: 'Error', className: 'bg-cp-error' },
]
const progressItems = computed(() => ([
  {
    label: t('appearance.preview.components.accountAvailability'),
    value: '94%',
    width: '94%',
  },
  {
    label: t('appearance.preview.components.todaySQuota'),
    value: '68%',
    width: '68%',
  },
  {
    label: t('appearance.preview.components.batchImport'),
    value: '12 / 20',
    width: '60%',
  },
]))
const tableColumns = computed(() => (defineTableColumns<PreviewTableRow>([
  {
    key: 'account',
    label: t('accounts.account'),
    kind: 'identity',
    size: '3xl',
  },
  {
    key: 'provider',
    label: t('appearance.preview.components.platform'),
    kind: 'status',
    size: 'md',
  },
  {
    key: 'status',
    label: t('common.fields.status'),
    kind: 'status',
    size: 'md',
  },
  {
    key: 'requests',
    label: t('appearance.preview.components.requestToday'),
    kind: 'numeric',
    size: 'lg',
  },
  {
    key: 'latency',
    label: t('usage.firstTokenLatency'),
    kind: 'numeric',
    size: 'lg',
  },
  {
    key: 'updatedAt',
    label: t('appearance.preview.components.recentlyUsed'),
    kind: 'datetime',
    size: 'lg',
  },
])))
const tableRows = computed<PreviewTableRow[]>(() => ([
  {
    id: 'relay',
    account: 'relay@example.com',
    provider: formatProviderLabel('openai'),
    status: 'active',
    requests: '12,840',
    latency: '228 ms',
    updatedAt: t('appearance.preview.components.justNow'),
  },
  {
    id: 'gateway',
    account: 'gateway@example.com',
    provider: formatProviderLabel('xai'),
    status: 'active',
    requests: '9,462',
    latency: '316 ms',
    updatedAt: t('common.time.minutesAgo', { count: 2 }),
  },
  {
    id: 'quota',
    account: 'quota@example.com',
    provider: formatProviderLabel('openai'),
    status: 'rate_limited',
    requests: '7,318',
    latency: '486 ms',
    updatedAt: t('common.time.minutesAgo', { count: 5 }),
  },
  {
    id: 'batch',
    account: 'batch@example.com',
    provider: formatProviderLabel('openai'),
    status: 'active',
    requests: '5,904',
    latency: '264 ms',
    updatedAt: t('common.time.minutesAgo', { count: 8 }),
  },
  {
    id: 'fallback',
    account: 'fallback@example.com',
    provider: formatProviderLabel('xai'),
    status: 'error',
    requests: '3,172',
    latency: '—',
    updatedAt: t('common.time.minutesAgo', { count: 12 }),
  },
  {
    id: 'disabled',
    account: 'disabled@example.com',
    provider: formatProviderLabel('openai'),
    status: 'disabled',
    requests: '0',
    latency: '—',
    updatedAt: t('appearance.preview.components.hoursAgo'),
  },
]))
const pagination: PaginationState = {
  currentPage: 2,
  pageSize: 20,
  total: 1_182,
}

const statusLabels = computed(() => ({
  active: t('common.status.healthy'),
  rate_limited: t('common.status.rateLimited'),
  disabled: t('common.status.disabled'),
  error: t('common.status.error'),
}))

function statusClass(status: PreviewTableRow['status']) {
  if (status === 'active')
    return 'bg-cp-success-container text-cp-success-on-container'
  if (status === 'rate_limited')
    return 'bg-cp-warning-container text-cp-warning-on-container'
  if (status === 'error')
    return 'bg-cp-error-container text-cp-error-on-container'
  return 'bg-cp-fill-tertiary text-cp-text-secondary'
}
</script>

<template>
  <div class="grid h-full min-w-0 grid-rows-[80px_352px_290px_520px_374px] gap-5 overflow-hidden rounded-cp-card bg-(--cp-card-bg) p-6 shadow-cp-card">
    <header class="flex items-center justify-between gap-8 px-1">
      <div class="min-w-0">
        <p class="m-0 font-mono text-cp-xs font-heavy tracking-[0.16em] text-cp-primary-text uppercase">
          {{ t('appearance.preview.components.eyebrow') }}
        </p>
        <h1 class="mt-2 mb-0 text-[30px] leading-none font-extrabold text-cp-text">
          {{ t('appearance.preview.components.componentOverview') }}
        </h1>
        <p class="mt-2 mb-0 text-cp font-emphasis text-cp-text-secondary">
          {{ t('appearance.preview.components.description') }}
        </p>
      </div>

      <div class="flex shrink-0 items-center gap-6">
        <span class="rounded-full bg-cp-primary-container px-3 py-1.5 font-mono text-[10px] font-heavy tracking-wide text-cp-primary-on-container uppercase">
          {{ t('appearance.preview.components.count', { count: 21 }) }}
        </span>
        <div class="flex items-center gap-5" :aria-label="t('appearance.preview.components.currentThemeSemanticColor')">
          <div
            v-for="item in themeSwatches"
            :key="item.label"
            class="grid justify-items-center gap-1.5"
          >
            <span class="size-6 rounded-cp shadow-cp-tertiary" :class="item.className" />
            <span class="font-mono text-[10px] font-heavy text-cp-text-quaternary">{{ item.label }}</span>
          </div>
        </div>
      </div>
    </header>

    <section class="grid min-w-0 grid-cols-12 gap-6" :aria-label="t('appearance.preview.components.basicControlComponents')">
      <ZCard padding="compact" class="col-span-7 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" :title="t('appearance.preview.components.buttonsAndActions')" description="ZButton · ZIconButton · ZMotionIcon">
        <template #body>
          <div class="grid flex-1 content-start gap-6">
            <div>
              <p class="mt-0 mb-3 font-mono text-cp-xs font-heavy text-cp-text-quaternary">
                {{ t('appearance.preview.components.buttonLabel') }}
              </p>
              <div class="flex flex-wrap items-center gap-3">
                <ZButton type="primary">
                  <template #icon>
                    <Plus class="size-4" />
                  </template>
                  {{ t('appearance.preview.components.createAccount') }}
                </ZButton>
                <ZButton>
                  <template #icon>
                    <Download class="size-4" />
                  </template>
                  {{ t('appearance.preview.components.exportData') }}
                </ZButton>
                <ZButton variant="text">
                  {{ t('appearance.preview.components.dealWithItLater') }}
                </ZButton>
                <ZButton type="danger" variant="plain">
                  <template #icon>
                    <Trash2 class="size-4" />
                  </template>
                  {{ t('common.actions.delete') }}
                </ZButton>
              </div>
            </div>

            <div class="grid grid-cols-[1fr_auto] items-end gap-5 rounded-cp-lg bg-cp-fill-quaternary p-4">
              <div>
                <p class="mt-0 mb-3 text-cp-sm font-heavy text-cp-text-secondary">
                  {{ t('appearance.preview.components.sizeAndDisabledStatus') }}
                </p>
                <div class="flex flex-wrap items-center gap-3">
                  <ZButton size="small" type="primary">
                    {{ t('common.sizes.small') }}
                  </ZButton>
                  <ZButton size="default" type="primary">
                    {{ t('common.sizes.medium') }}
                  </ZButton>
                  <ZButton size="large" type="primary">
                    {{ t('common.sizes.large') }}
                  </ZButton>
                  <ZButton disabled>
                    {{ t('appearance.preview.components.inoperable') }}
                  </ZButton>
                  <ZButton loading>
                    {{ t('appearance.preview.components.processing') }}
                  </ZButton>
                </div>
              </div>
              <div class="flex items-center gap-2">
                <ZIconButton :aria-label="t('appearance.preview.components.notification')" variant="solid">
                  <ZMotionIcon>
                    <Bell />
                  </ZMotionIcon>
                </ZIconButton>
                <ZIconButton :aria-label="t('appearance.preview.components.moreActions')" variant="solid">
                  <MoreHorizontal />
                </ZIconButton>
                <ZIconButton :aria-label="t('appearance.preview.components.deleteRecord')" type="danger">
                  <Trash2 />
                </ZIconButton>
              </div>
            </div>
          </div>
        </template>
      </ZCard>

      <ZCard padding="compact" class="col-span-5 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" :title="t('appearance.preview.components.formsAndInput')" description="ZInput · ZSelect · ZInputNumber · ZSlider · ZTextarea · ZColorPicker">
        <template #body>
          <ZForm class="grid-cols-2 gap-3!">
            <ZInput :model-value="search ?? t('appearance.preview.components.relayAccount')" :aria-label="t('appearance.preview.components.searchAccount')" :placeholder="t('appearance.preview.components.searchAccount')" @update:model-value="search = $event">
              <template #prefix>
                <Search class="size-4" />
              </template>
            </ZInput>
            <ZSelect v-model="provider" :aria-label="t('appearance.preview.components.choosePlatform')" :options="providerOptions" />
            <ZInput v-model="invalidKey" :aria-label="t('appearance.preview.components.accessKeyErrorExample')" aria-invalid="true">
              <template #prefix>
                <KeyRound class="size-4" />
              </template>
            </ZInput>
            <ZInput :model-value="t('appearance.preview.components.notEditable')" :aria-label="t('appearance.preview.components.disableInputBox')" disabled />
            <FormItem :label="t('appearance.preview.components.brandColors')" description="ZColorPicker">
              <ZColorPicker
                v-model="brandColor"
                :aria-label="t('appearance.preview.components.editBrandColors')"
                :allow-alpha="false"
                :presets="['#5983F4', '#0F766E', '#7C3AED', '#475569']"
              />
            </FormItem>
            <FormItem :label="t('appearance.preview.components.schedulingNotes')" description="ZTextarea">
              <ZTextarea :model-value="notes ?? t('appearance.preview.components.stickyHelp')" :aria-label="t('appearance.preview.components.schedulingNotes')" :rows="2" @update:model-value="notes = $event" />
            </FormItem>
            <FormItem class="col-span-2" :label="t('appearance.preview.components.controlHeight')" description="ZInputNumber · ZSlider">
              <div class="flex items-center gap-3">
                <ZInputNumber
                  v-model="controlHeight"
                  size="small"
                  :aria-label="t('appearance.preview.components.componentExampleControlHeight')"
                  :min="28"
                  :max="52"
                  unit="px"
                />
                <ZSlider
                  v-model="controlHeight"
                  class="min-w-0 flex-1"
                  :aria-label="t('appearance.preview.components.componentExampleControlHeight')"
                  :min="28"
                  :max="52"
                  unit="px"
                />
              </div>
            </FormItem>
          </ZForm>
        </template>
      </ZCard>
    </section>

    <section class="grid min-w-0 grid-cols-12 gap-6" :aria-label="t('appearance.preview.components.choiceAndFeedbackComponents')">
      <ZCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" :title="t('appearance.preview.components.selectAndSwitch')" description="ZSegmented · ZSwitch · ZCheckbox · ZRadio">
        <template #body>
          <div class="grid gap-5">
            <ZSegmented v-model="view" :aria-label="t('appearance.preview.components.accountView')" :options="viewOptions" class="w-full" />
            <div class="flex flex-wrap items-center gap-x-7 gap-y-4">
              <ZSwitch v-model="notifications" :label="t('appearance.preview.components.realTimeNotification')" />
              <ZSwitch v-model="autoRotate" :label="t('appearance.preview.components.automaticRotation')" />
              <ZCheckbox v-model="selected" :label="t('appearance.preview.components.selectCurrentPage')" />
              <ZCheckbox v-model="partialSelected" :label="t('appearance.preview.components.partialSelection')" indeterminate />
            </div>
            <div class="flex items-center gap-6">
              <ZRadio v-model="strategy" value="balanced" name="strategy" :label="t('appearance.preview.components.equilibrium')" />
              <ZRadio v-model="strategy" value="sticky" name="strategy" :label="t('appearance.preview.components.sticky')" />
              <ZRadio v-model="strategy" value="manual" name="strategy" :label="t('appearance.preview.components.manual')" />
            </div>
          </div>
        </template>
      </ZCard>

      <ZCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" :title="t('appearance.preview.components.feedbackSemantics')" :description="t('appearance.preview.components.feedbackTypes')">
        <template #body>
          <div class="grid gap-2.5">
            <div class="flex items-center gap-3 rounded-cp bg-cp-success-container px-3.5 py-3 text-cp-success-on-container">
              <Check class="size-4 shrink-0" /><strong class="text-cp-sm">{{ t('appearance.preview.components.themeSettingsSaved') }}</strong>
            </div>
            <div class="flex items-center gap-3 rounded-cp bg-cp-warning-container px-3.5 py-3 text-cp-warning-on-container">
              <TriangleAlert class="size-4 shrink-0" /><strong class="text-cp-sm">{{ t('appearance.preview.components.quotaOfAccountsIsCloseToUpper') }}</strong>
            </div>
            <div class="flex items-center gap-3 rounded-cp bg-cp-error-container px-3.5 py-3 text-cp-error-on-container">
              <CircleAlert class="size-4 shrink-0" /><strong class="text-cp-sm">{{ t('appearance.preview.components.connectionTestFailed') }}</strong>
            </div>
            <div class="flex items-center gap-3 rounded-cp bg-cp-info-container px-3.5 py-3 text-cp-info-on-container">
              <Info class="size-4 shrink-0" /><strong class="text-cp-sm">{{ t('appearance.preview.components.schedulingNotice') }}</strong>
            </div>
          </div>
        </template>
      </ZCard>

      <ZCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" :title="t('appearance.preview.components.statusAndProgress')" :description="t('appearance.preview.components.tagComponents')">
        <template #body>
          <div class="grid gap-4">
            <div class="flex flex-wrap gap-2">
              <ZTag type="success" round>
                {{ t('appearance.preview.components.runningNormally') }}
              </ZTag>
              <ZTag type="warning" round>
                {{ t('appearance.preview.components.quotaLimited') }}
              </ZTag>
              <ZTag type="danger" round>
                {{ t('appearance.preview.components.needToBeProcessed') }}
              </ZTag>
              <ZTag v-if="syncTagVisible" type="info" round closable @close="syncTagVisible = false">
                {{ t('appearance.preview.components.synchronizing') }}
              </ZTag>
              <ZButton v-else size="small" variant="text" @click="syncTagVisible = true">
                {{ t('appearance.preview.components.restoreTag') }}
              </ZButton>
            </div>
            <div v-for="item in progressItems" :key="item.label" class="grid gap-2">
              <div class="flex justify-between text-cp-sm font-heavy text-cp-text-secondary">
                <span>{{ item.label }}</span><span class="font-mono text-cp-text">{{ item.value }}</span>
              </div>
              <div class="h-2 overflow-hidden rounded-full bg-cp-progress-remaining">
                <div class="h-full rounded-full bg-cp-primary" :style="{ width: item.width }" />
              </div>
            </div>
          </div>
        </template>
      </ZCard>
    </section>

    <ZCard padding="compact" class="h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" :title="t('appearance.preview.components.dataTable')" :description="t('appearance.preview.components.tableComponents')">
      <template #actions>
        <ZInput :aria-label="t('appearance.preview.components.filterForm')" size="small" :placeholder="t('appearance.preview.components.filterAccounts')" class="w-64">
          <template #prefix>
            <Search class="size-3.5" />
          </template>
        </ZInput>
      </template>
      <template #body>
        <div class="grid min-h-0 flex-1 grid-rows-[minmax(0,1fr)_auto]">
          <ZTable :columns="tableColumns" :data="tableRows">
            <template #account="{ row }">
              <div class="grid gap-1">
                <strong class="font-mono text-cp-sm text-cp-text">{{ row.account }}</strong>
                <span class="text-[10px] font-emphasis text-cp-text-quaternary">credential_{{ row.id }}</span>
              </div>
            </template>
            <template #status="{ row }">
              <span class="inline-flex rounded-full px-2.5 py-1 text-cp-xs font-heavy" :class="statusClass(row.status)">
                {{ statusLabels[row.status] }}
              </span>
            </template>
          </ZTable>
          <ZPagination v-bind="pagination" :disabled="false" />
        </div>
      </template>
    </ZCard>

    <section class="grid min-w-0 grid-cols-12 gap-6 overflow-hidden" :aria-label="t('appearance.preview.components.boundaryStateComponent')">
      <ZCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" :title="t('appearance.preview.components.emptyState')" description="ZEmpty · ZButton">
        <template #body>
          <ZEmpty :title="t('appearance.preview.components.noMatchingAccountYet')" :description="t('appearance.preview.components.emptyHelp')" class="flex-1">
            <template #action>
              <ZButton size="small" type="primary">
                {{ t('appearance.preview.components.addAccount') }}
              </ZButton>
            </template>
          </ZEmpty>
        </template>
      </ZCard>

      <ZCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" :title="t('appearance.preview.components.loadingAndSkeleton')" :description="t('appearance.preview.components.loadingComponents')">
        <template #body>
          <div class="grid flex-1 content-between gap-5 rounded-cp-lg bg-cp-fill-quaternary p-4">
            <div class="grid gap-4">
              <div class="flex items-center gap-3">
                <ZSkeleton class="size-10 shrink-0" />
                <div class="grid flex-1 gap-2">
                  <ZSkeleton shape="text" class="w-2/5" />
                  <ZSkeleton shape="text" class="h-2.5 w-3/5 opacity-60" />
                </div>
              </div>
              <ZSkeleton class="h-18 opacity-60" />
              <div class="grid grid-cols-3 gap-3">
                <ZSkeleton v-for="index in 3" :key="index" class="h-12 opacity-60" />
              </div>
            </div>
            <div class="flex items-center justify-between">
              <span class="text-cp-sm font-emphasis text-cp-text-secondary">{{ t('appearance.preview.components.synchronizingAccountStatus') }}</span>
              <ZButton size="small" loading>
                {{ t('appearance.preview.components.synchronizing') }}
              </ZButton>
            </div>
          </div>
        </template>
      </ZCard>

      <ZCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" :title="t('appearance.preview.components.overlaysAndMenus')" :description="t('appearance.preview.components.overlayComponents')">
        <template #body>
          <div class="grid min-w-0 flex-1 grid-rows-[auto_1fr_auto] overflow-hidden rounded-cp-lg bg-cp-bg-container p-4">
            <div class="flex items-center justify-between rounded-cp bg-cp-bg-container px-4 py-3 shadow-cp-tertiary">
              <div>
                <strong class="block text-cp-sm text-cp-text">relay@example.com</strong>
                <span class="mt-1 block text-[10px] text-cp-text-quaternary">{{ formatProviderLabel('openai') }} · OAuth</span>
              </div>
              <ZPopover>
                <template #reference>
                  <ZIconButton :aria-label="t('appearance.preview.components.accountOperation')" variant="solid">
                    <MoreHorizontal />
                  </ZIconButton>
                </template>
                <div class="w-64 max-w-full p-2">
                  <ZMenuItem>{{ t('appearance.preview.components.editAccount') }}</ZMenuItem>
                  <ZMenuItem>{{ t('accounts.connectionTest.title') }}</ZMenuItem>
                  <ZMenuItem type="danger">
                    {{ t('appearance.preview.components.deactivateAccount') }}
                  </ZMenuItem>
                </div>
              </ZPopover>
            </div>

            <div class="mt-3 ml-auto w-64 max-w-full self-start rounded-cp-lg bg-cp-bg-elevated p-2 shadow-cp">
              <ZMenuItem>
                <template #icon>
                  <KeyRound class="size-3.5" />
                </template>
                {{ t('appearance.preview.components.editAccount') }}
              </ZMenuItem>
              <ZMenuItem>
                <template #icon>
                  <Info class="size-3.5" />
                </template>
                {{ t('accounts.connectionTest.title') }}
              </ZMenuItem>
              <ZMenuItem type="danger">
                <template #icon>
                  <Trash2 class="size-3.5" />
                </template>
                {{ t('appearance.preview.components.deactivateAccount') }}
              </ZMenuItem>
            </div>

            <div class="flex min-w-0 items-center gap-3 rounded-cp bg-cp-bg-spotlight px-4 py-3 text-cp-text-light-solid shadow-cp-popup">
              <Info class="size-4 shrink-0" />
              <span class="min-w-0 text-cp-xs font-heavy">{{ t('appearance.preview.components.overlayHelp') }}</span>
            </div>
          </div>
        </template>
      </ZCard>
    </section>
  </div>
</template>
