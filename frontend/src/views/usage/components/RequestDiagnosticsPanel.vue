<script setup lang="ts">
import type { OpsError, OpsErrorMetadata } from '@/api'
import { Download, RefreshCw } from '@lucide/vue'
import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useDownload } from '@/composables/useDownload'
import { useRequestDiagnostics } from '../composables/useRequestDiagnostics'
import { requestDiagnosticsBundle } from '../utils/diagnosticsBundle'
import RequestTransportFailure from './RequestTransportFailure.vue'
import UsageDetailCodePanel from './UsageDetailCodePanel.vue'

const props = defineProps<{ requestId: string, metadata?: OpsErrorMetadata, errorRecord?: OpsError }>()
const { downloadJson } = useDownload()
const { loading: exporting, run: runExport } = useAsyncAction()
const { selectedId, detail, loading, error, refresh } = useRequestDiagnostics(() => props.requestId)
const trace = computed(() => detail.value?.trace)
const events = computed(() => (trace.value?.events ?? []).map(event => ({
  ...event,
  content: JSON.stringify(event.data, null, 2),
})))
const selectedError = computed(() => props.errorRecord?.requestId === selectedId.value ? props.errorRecord : undefined)
const canExport = computed(() => !loading.value && (detail.value?.requestId === selectedId.value || !!selectedError.value))

function download() {
  if (!canExport.value || exporting.value)
    return
  const bundle = requestDiagnosticsBundle(selectedId.value, detail.value, selectedError.value)
  return runExport(
    () => downloadJson(bundle, `diagnostics-${selectedId.value.replace(/[^\w-]/g, '_')}.json`),
    { errorText: '导出诊断包失败', minimumMs: 400 },
  )
}
</script>

<template>
  <section class="mt-3 min-w-0 rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5" aria-label="请求诊断">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <h3 class="m-0 text-cp-sm font-heavy text-cp-text-secondary">
        请求诊断
      </h3>
      <div class="flex flex-wrap gap-2">
        <BaseButton v-if="selectedId !== requestId" variant="soft" size="sm" @click="selectedId = requestId">
          返回本次请求
        </BaseButton>
        <BaseButton variant="soft" size="sm" :loading="loading" @click="refresh">
          <template #icon>
            <RefreshCw :size="14" />
          </template>
          刷新
        </BaseButton>
        <BaseButton variant="soft" size="sm" :loading="exporting" :disabled="!canExport" @click="download">
          <template #icon>
            <Download :size="14" />
          </template>
          导出安全诊断包
        </BaseButton>
      </div>
    </div>
    <p class="mt-1 mb-3 break-all font-mono text-cp-xs leading-relaxed text-cp-text-secondary">
      {{ selectedId }}
    </p>
    <p class="mb-3 text-cp-xs leading-relaxed text-cp-text-secondary">
      诊断包 v2 仅含关联字段、错误分类、尝试与时间线阶段；不含错误原文、正文、用户内容或事件 data。版本与环境需另附，分享前请审阅关联 ID。
    </p>
    <p v-if="!loading && !selectedError" class="text-cp-xs leading-relaxed text-cp-text-secondary">
      当前请求未提供错误详情摘要，导出不会沿用其他请求的错误；可结合尝试记录排查。
    </p>
    <p v-if="loading" role="status" class="text-cp-sm text-cp-text-secondary">
      正在加载诊断记录…
    </p>
    <p v-else-if="error" role="alert" class="text-cp-sm text-cp-error-text">
      {{ error }}
      <span v-if="selectedError">仍可导出本次错误的分类摘要和关联字段，缺失诊断会在包内标明。</span>
    </p>
    <template v-else-if="detail">
      <p v-if="!detail.attemptsComplete" role="status" class="text-cp-xs leading-relaxed text-cp-warning-text">
        尝试记录可能不完整，诊断包会保留此缺口，不能仅按列表条数判断实际尝试次数。
      </p>
      <div v-if="detail.relatedRequests?.length" class="mb-3 flex flex-wrap gap-2">
        <BaseButton v-for="related in detail.relatedRequests" :key="related.requestId" variant="soft" size="sm" class="max-w-full" @click="selectedId = related.requestId">
          {{ related.relation === 'recovered_by' ? '查看恢复请求' : '查看先前失败' }} · {{ related.requestId }}
        </BaseButton>
      </div>
      <RequestTransportFailure :events="trace?.events ?? []" :metadata="selectedId === requestId ? metadata : undefined" />
      <p v-if="!trace" class="text-cp-sm text-cp-text-secondary">
        这条记录没有保存诊断时间线。旧记录无法补回当时未采集的事件。
      </p>
      <template v-else>
        <div class="mb-3 grid gap-1 text-cp-xs leading-relaxed">
          <p class="m-0 text-cp-text-secondary">
            已观测 {{ trace.totalEvents }} 个阶段或事件，展示 {{ events.length }} 条记录。连续增量合并计数；历史事件内容可能未充分脱敏，不会自动导出。
          </p>
          <p v-if="trace.droppedEvents" role="status" class="m-0 text-cp-warning-text">
            达到保存上限，{{ trace.droppedEvents }} 个事件已被淘汰；保留请求开头与最近事件。
          </p>
        </div>
        <BaseScrollbar max-height="32rem">
          <ol class="m-0 grid list-none gap-2 p-0 pr-3">
            <li v-for="event in events" :key="event.sequence" class="min-w-0">
              <details class="group overflow-hidden rounded-cp bg-cp-bg-container">
                <summary class="cursor-pointer px-3 py-2.5 break-all text-cp-xs leading-relaxed transition-colors hover:bg-cp-primary-container group-open:bg-cp-primary-container focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-cp-primary motion-reduce:transition-none">
                  <span class="font-mono tabular-nums text-cp-text-secondary">+{{ event.elapsedMs }} ms · #{{ event.sequence }}</span>
                  <span class="mx-2 font-mono text-cp-text">{{ event.stage }}</span>
                  <span v-if="event.attemptIndex" class="text-cp-text-secondary">尝试 {{ event.attemptIndex }}</span>
                  <span v-if="event.exchangeId" class="text-cp-text-secondary"> · 交换 {{ event.exchangeId }}</span>
                  <span v-if="event.count > 1" class="text-cp-text-secondary"> · {{ event.count }} 次（至 +{{ event.lastElapsedMs }} ms）</span>
                </summary>
                <div class="mx-3 py-3">
                  <UsageDetailCodePanel title="诊断事实" :content="event.content" max-height="280px" />
                </div>
              </details>
            </li>
          </ol>
        </BaseScrollbar>
        <p class="mt-3 mb-0 text-cp-xs leading-relaxed text-cp-text-secondary">
          界面中的诊断事实不等于默认导出内容。原始错误或完整报文如确需反馈，请先单独审阅脱敏；可用请求 ID 在服务器日志中检索。
        </p>
      </template>
    </template>
  </section>
</template>
