//! 额度预测投影与历史采样的模块门面

mod projection;
mod sampling;

pub(crate) use projection::quota_forecast_source_window;
pub use projection::{
    AccountQuotaForecast, AccountQuotaForecastReport, QuotaForecastSource, account_quota_forecasts,
};
pub use sampling::{
    MAX_FORECAST_HISTORY_POINTS, QuotaForecastHistory, QuotaForecastHistoryPoint,
    QuotaForecastMethod, QuotaForecastObservation, QuotaForecastPoint, QuotaForecastSample,
    QuotaForecastUsage, select_forecast_sample,
};
