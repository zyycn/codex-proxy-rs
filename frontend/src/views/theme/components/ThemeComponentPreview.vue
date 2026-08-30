<script setup lang="ts">
import type { BaseTablePagination as BaseTablePaginationState } from '@/components/base/BaseTable/pagination'

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
import { shallowRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseColorPicker from '@/components/base/BaseColorPicker/index.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import FormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseMenuItem from '@/components/base/BaseMenuItem.vue'
import BaseMotionIcon from '@/components/base/BaseMotionIcon.vue'
import BaseNumberInput from '@/components/base/BaseNumberInput.vue'
import BasePopover from '@/components/base/BasePopover.vue'
import BaseRadio from '@/components/base/BaseRadio.vue'
import BaseRange from '@/components/base/BaseRange.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'

interface PreviewTableRow {
  id: string
  account: string
  provider: string
  status: '正常' | '限流' | '停用' | '异常'
  requests: string
  latency: string
  updatedAt: string
}

const search = shallowRef('中继账号')
const notes = shallowRef('优先保持当前账号，额度耗尽后再切换。')
const brandColor = shallowRef('#5983F4')
const invalidKey = shallowRef('sk-invalid')
const disabledText = shallowRef('不可编辑')
const provider = shallowRef('openai')
const view = shallowRef('all')
const notifications = shallowRef(true)
const autoRotate = shallowRef(false)
const selected = shallowRef(true)
const partialSelected = shallowRef(false)
const strategy = shallowRef('balanced')
const controlHeight = shallowRef(38)

const providerOptions = [
  { label: 'OpenAI', value: 'openai' },
  { label: 'xAI', value: 'xai' },
  { label: '全部平台', value: 'all' },
]
const viewOptions = [
  { label: '全部', value: 'all' },
  { label: '正常', value: 'active' },
  { label: '受限', value: 'limited' },
]
const themeSwatches = [
  { label: 'Container', className: 'bg-cp-bg-container' },
  { label: 'Fill Alter', className: 'bg-cp-fill-alter' },
  { label: 'Elevated', className: 'bg-cp-bg-elevated' },
  { label: 'Accent', className: 'bg-cp-primary' },
  { label: 'Success', className: 'bg-cp-success' },
  { label: 'Warning', className: 'bg-cp-warning' },
  { label: 'Error', className: 'bg-cp-error' },
]
const progressItems = [
  { label: '账号可用率', value: '94%', width: '94%' },
  { label: '今日额度', value: '68%', width: '68%' },
  { label: '批量导入', value: '12 / 20', width: '60%' },
]
const tableColumns = defineTableColumns<PreviewTableRow>([
  { key: 'account', label: '账号', kind: 'identity', size: '3xl' },
  { key: 'provider', label: '平台', kind: 'status', size: 'md' },
  { key: 'status', label: '状态', kind: 'status', size: 'md' },
  { key: 'requests', label: '今日请求', kind: 'numeric', size: 'lg' },
  { key: 'latency', label: '首字延迟', kind: 'numeric', size: 'lg' },
  { key: 'updatedAt', label: '最近使用', kind: 'datetime', size: 'lg' },
])
const tableRows: PreviewTableRow[] = [
  { id: 'relay', account: 'relay@example.com', provider: 'OpenAI', status: '正常', requests: '12,840', latency: '228 ms', updatedAt: '刚刚' },
  { id: 'gateway', account: 'gateway@example.com', provider: 'xAI', status: '正常', requests: '9,462', latency: '316 ms', updatedAt: '2 分钟前' },
  { id: 'quota', account: 'quota@example.com', provider: 'OpenAI', status: '限流', requests: '7,318', latency: '486 ms', updatedAt: '5 分钟前' },
  { id: 'batch', account: 'batch@example.com', provider: 'OpenAI', status: '正常', requests: '5,904', latency: '264 ms', updatedAt: '8 分钟前' },
  { id: 'fallback', account: 'fallback@example.com', provider: 'xAI', status: '异常', requests: '3,172', latency: '—', updatedAt: '12 分钟前' },
  { id: 'disabled', account: 'disabled@example.com', provider: 'OpenAI', status: '停用', requests: '0', latency: '—', updatedAt: '2 小时前' },
]
const pagination: BaseTablePaginationState = {
  currentPage: 2,
  pageSize: 20,
  total: 1_182,
}

function statusClass(status: PreviewTableRow['status']) {
  if (status === '正常')
    return 'bg-cp-success-bg text-cp-success-text'
  if (status === '限流')
    return 'bg-cp-warning-bg text-cp-warning-text'
  if (status === '异常')
    return 'bg-cp-error-bg text-cp-error-text'
  return 'bg-cp-fill-tertiary text-cp-text-secondary'
}
</script>

<template>
  <div class="grid h-full min-w-0 grid-rows-[80px_352px_290px_520px_374px] gap-5 overflow-hidden rounded-cp-card bg-(--cp-card-bg) p-6 shadow-cp-card">
    <header class="flex items-center justify-between gap-8 px-1">
      <div class="min-w-0">
        <p class="m-0 font-mono text-cp-xs font-heavy tracking-[0.16em] text-cp-primary-text uppercase">
          Component specimen
        </p>
        <h1 class="mt-2 mb-0 text-[30px] leading-none font-extrabold text-cp-text">
          组件概览
        </h1>
        <p class="mt-2 mb-0 text-cp font-emphasis text-cp-text-secondary">
          在同一张校样板中检查控制态、数据密度与浮层关系
        </p>
      </div>

      <div class="flex shrink-0 items-center gap-6">
        <span class="rounded-full bg-cp-primary-bg px-3 py-1.5 font-mono text-[10px] font-heavy tracking-wide text-cp-primary-text uppercase">
          20 Base Components
        </span>
        <div class="flex items-center gap-5" aria-label="当前主题语义色">
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

    <section class="grid min-w-0 grid-cols-12 gap-6" aria-label="基础控制组件">
      <BaseCard padding="compact" class="col-span-7 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" title="按钮与动作" description="BaseButton · BaseIconButton · BaseMotionIcon">
        <template #body>
          <div class="grid flex-1 content-start gap-6">
            <div>
              <p class="mt-0 mb-3 font-mono text-cp-xs font-heavy text-cp-text-quaternary">
                Button / action
              </p>
              <div class="flex flex-wrap items-center gap-3">
                <BaseButton variant="primary">
                  <template #icon>
                    <Plus class="size-4" />
                  </template>
                  创建账号
                </BaseButton>
                <BaseButton variant="secondary">
                  <template #icon>
                    <Download class="size-4" />
                  </template>
                  导出数据
                </BaseButton>
                <BaseButton variant="ghost">
                  稍后处理
                </BaseButton>
                <BaseButton variant="destructive">
                  <template #icon>
                    <Trash2 class="size-4" />
                  </template>
                  删除
                </BaseButton>
              </div>
            </div>

            <div class="grid grid-cols-[1fr_auto] items-end gap-5 rounded-cp-lg bg-cp-fill-quaternary p-4">
              <div>
                <p class="mt-0 mb-3 text-cp-sm font-heavy text-cp-text-secondary">
                  尺寸与禁用状态
                </p>
                <div class="flex flex-wrap items-center gap-3">
                  <BaseButton size="sm" variant="primary">
                    Small
                  </BaseButton>
                  <BaseButton size="md" variant="primary">
                    Medium
                  </BaseButton>
                  <BaseButton size="lg" variant="primary">
                    Large
                  </BaseButton>
                  <BaseButton disabled>
                    不可操作
                  </BaseButton>
                  <BaseButton loading>
                    处理中
                  </BaseButton>
                </div>
              </div>
              <div class="flex items-center gap-2">
                <BaseIconButton label="通知" variant="secondary">
                  <BaseMotionIcon>
                    <Bell />
                  </BaseMotionIcon>
                </BaseIconButton>
                <BaseIconButton label="更多操作" variant="secondary">
                  <MoreHorizontal />
                </BaseIconButton>
                <BaseIconButton label="删除记录" variant="destructive">
                  <Trash2 />
                </BaseIconButton>
              </div>
            </div>
          </div>
        </template>
      </BaseCard>

      <BaseCard padding="compact" class="col-span-5 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" title="表单与输入" description="BaseInput · BaseSelect · BaseNumberInput · BaseRange · BaseTextarea · BaseColorPicker">
        <template #body>
          <BaseForm class="grid-cols-2 gap-3!">
            <BaseInput v-model="search" aria-label="搜索账号" placeholder="搜索账号">
              <template #prefix>
                <Search class="size-4" />
              </template>
            </BaseInput>
            <BaseSelect v-model="provider" aria-label="选择平台" :options="providerOptions" />
            <BaseInput v-model="invalidKey" aria-label="访问密钥错误示例" aria-invalid="true">
              <template #prefix>
                <KeyRound class="size-4" />
              </template>
            </BaseInput>
            <BaseInput v-model="disabledText" aria-label="禁用输入框" disabled />
            <FormItem label="品牌颜色" description="BaseColorPicker">
              <BaseColorPicker
                v-model="brandColor"
                label="编辑品牌颜色"
                :allow-alpha="false"
                :presets="['#5983F4', '#0F766E', '#7C3AED', '#475569']"
              />
            </FormItem>
            <FormItem label="调度备注" description="BaseTextarea">
              <BaseTextarea v-model="notes" aria-label="调度备注" :rows="2" />
            </FormItem>
            <FormItem class="col-span-2" label="控件高度" description="BaseNumberInput · BaseRange">
              <div class="flex items-center gap-3">
                <BaseNumberInput
                  v-model="controlHeight"
                  label="组件示例控件高度"
                  :min="28"
                  :max="52"
                  unit="px"
                />
                <BaseRange
                  v-model="controlHeight"
                  class="min-w-0 flex-1"
                  label="组件示例控件高度"
                  :min="28"
                  :max="52"
                  unit="px"
                />
              </div>
            </FormItem>
          </BaseForm>
        </template>
      </BaseCard>
    </section>

    <section class="grid min-w-0 grid-cols-12 gap-6" aria-label="选择与反馈组件">
      <BaseCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" title="选择与切换" description="BaseSegmented · BaseSwitch · BaseCheckbox · BaseRadio">
        <template #body>
          <div class="grid gap-5">
            <BaseSegmented v-model="view" label="账号视图" :options="viewOptions" class="w-full" />
            <div class="flex flex-wrap items-center gap-x-7 gap-y-4">
              <BaseSwitch v-model="notifications" label="实时通知" show-label />
              <BaseSwitch v-model="autoRotate" label="自动轮换" show-label />
              <BaseCheckbox v-model="selected" label="选择当前页" show-label />
              <BaseCheckbox v-model="partialSelected" label="部分选择" indeterminate show-label />
            </div>
            <div class="flex items-center gap-6">
              <BaseRadio v-model="strategy" value="balanced" name="strategy" label="均衡" show-label />
              <BaseRadio v-model="strategy" value="sticky" name="strategy" label="粘性" show-label />
              <BaseRadio v-model="strategy" value="manual" name="strategy" label="手动" show-label />
            </div>
          </div>
        </template>
      </BaseCard>

      <BaseCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" title="反馈语义" description="Success · Warning · Error · Info">
        <template #body>
          <div class="grid gap-2.5">
            <div class="flex items-center gap-3 rounded-cp bg-cp-success-bg px-3.5 py-3 text-cp-success-text">
              <Check class="size-4 shrink-0" /><strong class="text-cp-sm">主题设置已保存</strong>
            </div>
            <div class="flex items-center gap-3 rounded-cp bg-cp-warning-bg px-3.5 py-3 text-cp-warning-text">
              <TriangleAlert class="size-4 shrink-0" /><strong class="text-cp-sm">3 个账号额度接近上限</strong>
            </div>
            <div class="flex items-center gap-3 rounded-cp bg-cp-error-bg px-3.5 py-3 text-cp-error-text">
              <CircleAlert class="size-4 shrink-0" /><strong class="text-cp-sm">连接测试未通过</strong>
            </div>
            <div class="flex items-center gap-3 rounded-cp bg-cp-info-bg px-3.5 py-3 text-cp-info-text">
              <Info class="size-4 shrink-0" /><strong class="text-cp-sm">调度规则将在下次请求生效</strong>
            </div>
          </div>
        </template>
      </BaseCard>

      <BaseCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" title="状态与进度" description="Alias Token · Progress Token">
        <template #body>
          <div class="grid gap-4">
            <div class="flex flex-wrap gap-2">
              <span class="rounded-full bg-cp-success-bg px-3 py-1.5 text-cp-xs font-heavy text-cp-success-text">运行正常</span>
              <span class="rounded-full bg-cp-warning-bg px-3 py-1.5 text-cp-xs font-heavy text-cp-warning-text">额度受限</span>
              <span class="rounded-full bg-cp-error-bg px-3 py-1.5 text-cp-xs font-heavy text-cp-error-text">需要处理</span>
              <span class="rounded-full bg-cp-info-bg px-3 py-1.5 text-cp-xs font-heavy text-cp-info-text">同步中</span>
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
      </BaseCard>
    </section>

    <BaseCard padding="compact" class="h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" title="数据表格" description="BaseTable · BaseScrollbar · BaseTablePagination（标准行高）">
      <template #actions>
        <BaseInput aria-label="筛选表格" size="sm" placeholder="筛选账号" class="w-64">
          <template #prefix>
            <Search class="size-3.5" />
          </template>
        </BaseInput>
      </template>
      <template #body>
        <div class="grid min-h-0 flex-1 grid-rows-[minmax(0,1fr)_auto]">
          <BaseTable :columns="tableColumns" :rows="tableRows">
            <template #account="{ row }">
              <div class="grid gap-1">
                <strong class="font-mono text-cp-sm text-cp-text">{{ row.account }}</strong>
                <span class="text-[10px] font-emphasis text-cp-text-quaternary">credential_{{ row.id }}</span>
              </div>
            </template>
            <template #status="{ row }">
              <span class="inline-flex rounded-full px-2.5 py-1 text-cp-xs font-heavy" :class="statusClass(row.status)">
                {{ row.status }}
              </span>
            </template>
          </BaseTable>
          <BaseTablePagination :pagination="pagination" :loading="false" />
        </div>
      </template>
    </BaseCard>

    <section class="grid min-w-0 grid-cols-12 gap-6 overflow-hidden" aria-label="边界状态组件">
      <BaseCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" title="空状态" description="BaseEmpty · BaseButton">
        <template #body>
          <BaseEmpty title="暂无匹配账号" description="调整筛选条件，或添加一个新的上游账号。" class="flex-1">
            <template #action>
              <BaseButton size="sm" variant="primary">
                添加账号
              </BaseButton>
            </template>
          </BaseEmpty>
        </template>
      </BaseCard>

      <BaseCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" title="加载与骨架" description="BaseSkeleton · BaseButton loading">
        <template #body>
          <div class="grid flex-1 content-between gap-5 rounded-cp-lg bg-cp-fill-quaternary p-4">
            <div class="grid gap-4">
              <div class="flex items-center gap-3">
                <BaseSkeleton class="size-10 shrink-0" />
                <div class="grid flex-1 gap-2">
                  <BaseSkeleton shape="text" class="w-2/5" />
                  <BaseSkeleton shape="text" class="h-2.5 w-3/5 opacity-60" />
                </div>
              </div>
              <BaseSkeleton class="h-18 opacity-60" />
              <div class="grid grid-cols-3 gap-3">
                <BaseSkeleton v-for="index in 3" :key="index" class="h-12 opacity-60" />
              </div>
            </div>
            <div class="flex items-center justify-between">
              <span class="text-cp-sm font-emphasis text-cp-text-secondary">正在同步账号状态</span>
              <BaseButton size="sm" loading>
                同步中
              </BaseButton>
            </div>
          </div>
        </template>
      </BaseCard>

      <BaseCard padding="compact" class="col-span-4 h-full min-w-0 bg-cp-fill-quaternary! shadow-none!" title="浮层与菜单" description="BasePopover · BaseMenuItem · Elevated Surface">
        <template #body>
          <div class="grid min-w-0 flex-1 grid-rows-[auto_1fr_auto] overflow-hidden rounded-cp-lg bg-cp-bg-container p-4">
            <div class="flex items-center justify-between rounded-cp bg-cp-bg-container px-4 py-3 shadow-cp-tertiary">
              <div>
                <strong class="block text-cp-sm text-cp-text">relay@example.com</strong>
                <span class="mt-1 block text-[10px] text-cp-text-quaternary">OpenAI · OAuth</span>
              </div>
              <BasePopover>
                <template #trigger>
                  <BaseIconButton label="账号操作" variant="secondary">
                    <MoreHorizontal />
                  </BaseIconButton>
                </template>
                <div class="w-64 max-w-full p-2">
                  <BaseMenuItem>编辑账号</BaseMenuItem>
                  <BaseMenuItem>测试连接</BaseMenuItem>
                  <BaseMenuItem tone="destructive">
                    停用账号
                  </BaseMenuItem>
                </div>
              </BasePopover>
            </div>

            <div class="mt-3 ml-auto w-64 max-w-full self-start rounded-cp-lg bg-cp-bg-elevated p-2 shadow-cp">
              <BaseMenuItem>
                <template #icon>
                  <KeyRound class="size-3.5" />
                </template>
                编辑账号
              </BaseMenuItem>
              <BaseMenuItem>
                <template #icon>
                  <Info class="size-3.5" />
                </template>
                测试连接
              </BaseMenuItem>
              <BaseMenuItem tone="destructive">
                <template #icon>
                  <Trash2 class="size-3.5" />
                </template>
                停用账号
              </BaseMenuItem>
            </div>

            <div class="flex min-w-0 items-center gap-3 rounded-cp bg-cp-bg-spotlight px-4 py-3 text-cp-text-light-solid shadow-cp-popup">
              <Info class="size-4 shrink-0" />
              <span class="min-w-0 text-cp-xs font-heavy">浮层始终使用独立 Surface 与 Shadow Token</span>
            </div>
          </div>
        </template>
      </BaseCard>
    </section>
  </div>
</template>
