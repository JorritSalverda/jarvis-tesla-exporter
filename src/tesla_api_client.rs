use std::error::Error;

use chrono::Utc;
use jarvis_lib::model::{EntityType, MetricType, Sample, SampleType};
use jarvis_lib::{measurement_client::MeasurementClient, model::Measurement};
use log::{debug, info};
use retry::delay::{jitter, Exponential};
use retry::retry;
use uuid::Uuid;

use crate::model::{
    Config, TeslaAccessToken, TeslaAccessTokenRequest, TeslaApiResponse, TeslaVehicle,
    TeslaVehicleData, TeslaVehicleState,
};

const RETRY_INTERVAL_MS: u64 = 100;
const RETRY_FACTOR: f64 = 2.0;
const RETRY_TAKES: usize = 3;

pub struct TeslaApiClient {}

impl MeasurementClient<Config> for TeslaApiClient {
    fn get_measurement(
        &self,
        config: Config,
        _last_measurement: Option<Measurement>,
    ) -> Result<Measurement, Box<dyn Error>> {
        let token = self.get_access_token(&config)?;

        let vehicles = self.get_vehicles(&token)?;

        for vehicle in vehicles {
            debug!(
                "State for vehicle {}: {:?}",
                vehicle.display_name, vehicle.state
            );

            if vehicle.in_service || vehicle.state == TeslaVehicleState::Asleep {
                info!("Vehicle is in service or asleep, skip storing samples");
                continue;
            }

            let vehicle_data = self.get_vehicle_data(&token, &vehicle)?;

            if let Some(geofence) = vehicle_data.in_geofence(&config.geofences) {
                info!(
                    "Vehicle is inside geofence {}, storing samples",
                    geofence.location
                );

                let mut measurement = Measurement {
                    id: Uuid::new_v4().to_string(),
                    source: String::from("jarvis-tesla-exporter"),
                    location: geofence.location,
                    samples: Vec::new(),
                    measured_at_time: Utc::now(),
                };

                // store as gauge for timeline graphs
                measurement.samples.push(Sample {
                    entity_type: EntityType::Device,
                    entity_name: "jarvis-tesla-exporter".into(),
                    sample_type: SampleType::ElectricityConsumption,
                    sample_name: vehicle.display_name.clone(),
                    metric_type: MetricType::Gauge,
                    value: vehicle_data.charge_state.charger_power * 1000.0,
                });

                // store as counter for totals
                measurement.samples.push(Sample {
                    entity_type: EntityType::Device,
                    entity_name: "jarvis-tesla-exporter".into(),
                    sample_type: SampleType::ElectricityConsumption,
                    sample_name: vehicle.display_name,
                    metric_type: MetricType::Counter,
                    value: vehicle_data.charge_state.charge_energy_added * 1000.0 * 3600.0,
                });

                return Ok(measurement);
            } else {
                info!("Vehicle is not inside geofence, skip storing samples");
                continue;
            }
        }

        Err(Box::<dyn Error>::from(
            "No vehicles were inside of any of the configured geofences and awake",
        ))
    }
}

impl TeslaApiClient {
    pub fn new() -> Self {
        Self {}
    }

    pub fn get_access_token(
        &self,
        config: &Config,
    ) -> Result<TeslaAccessToken, Box<dyn std::error::Error>> {
        info!("Fetching access token...");
        let url = "https://auth.tesla.com/oauth2/v3/token";

        debug!("POST {}", url);

        let request_body: TeslaAccessTokenRequest = TeslaAccessTokenRequest {
            grant_type: "refresh_token".into(),
            scope: "openid email offline_access".into(),
            client_id: "ownerapi".into(),
            refresh_token: config.refresh_token.clone(),
        };

        let access_token: TeslaAccessToken = retry(
            Exponential::from_millis_with_factor(RETRY_INTERVAL_MS, RETRY_FACTOR)
                .map(jitter)
                .take(RETRY_TAKES),
            || {
                reqwest::blocking::Client::new()
                    .post(url)
                    .json(&request_body)
                    .send()
            },
        )?
        .json()?;

        Ok(access_token)
    }

    pub fn get_vehicles(
        &self,
        token: &TeslaAccessToken,
    ) -> Result<Vec<TeslaVehicle>, Box<dyn std::error::Error>> {
        info!("Fetching vehicles...");
        let url = "https://owner-api.teslamotors.com/api/1/vehicles";

        debug!("GET {}", url);

        let vehicles_response: TeslaApiResponse<Vec<TeslaVehicle>> = retry(
            Exponential::from_millis_with_factor(RETRY_INTERVAL_MS, RETRY_FACTOR)
                .map(jitter)
                .take(RETRY_TAKES),
            || {
                reqwest::blocking::Client::new()
                    .get(url)
                    .bearer_auth(token.access_token.clone())
                    .send()
            },
        )?
        .json()?;

        Ok(vehicles_response.response)
    }

    pub fn get_vehicle_data(
        &self,
        token: &TeslaAccessToken,
        vehicle: &TeslaVehicle,
    ) -> Result<TeslaVehicleData, Box<dyn std::error::Error>> {
        info!("Fetching vehicle date for {}...", vehicle.display_name);
        let url = format!(
            "https://owner-api.teslamotors.com/api/1/vehicles/{}/vehicle_data",
            vehicle.id
        );

        debug!("GET {}", url);

        let vehicle_data_response: TeslaApiResponse<TeslaVehicleData> = retry(
            Exponential::from_millis_with_factor(RETRY_INTERVAL_MS, RETRY_FACTOR)
                .map(jitter)
                .take(RETRY_TAKES),
            || {
                reqwest::blocking::Client::new()
                    .get(&url)
                    .bearer_auth(token.access_token.clone())
                    .send()
            },
        )?
        .json()?;

        // on error returns
        // {"response":null,"error":"vehicle unavailable: {:error=>\"vehicle unavailable:\"}","error_description":""}

        Ok(vehicle_data_response.response)
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use crate::model::GeofenceConfig;

    use super::*;

    #[test]
    #[ignore]
    fn get_vehicle_data() {
        let tesla_api_client = TeslaApiClient::new();

        let refresh_token = env::var("TESLA_AUTH_REFRESH_TOKEN")
            .expect("Environment variable TESLA_AUTH_REFRESH_TOKEN not set");

        let config: Config = Config {
            refresh_token: refresh_token,
            geofences: vec![GeofenceConfig {
                location: "My Home".into(),
                latitude: 0.0,
                longitude: 0.0,
                geofence_radius_meters: 100.0,
            }],
        };

        // act
        let token = tesla_api_client
            .get_access_token(&config)
            .expect("Failed getting access token");

        let vehicles = tesla_api_client
            .get_vehicles(&token)
            .expect("Failed retrieving vehicles");

        for vehicle in vehicles {
            let vehicle_charge_state = tesla_api_client
                .get_vehicle_data(&token, &vehicle)
                .expect("Failed getting vehicle charge state");

            debug!("{:?}", vehicle_charge_state);
        }
    }
}
