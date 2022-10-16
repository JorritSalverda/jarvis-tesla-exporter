use std::error::Error;
use std::time::Instant;

use chrono::Utc;
use jarvis_lib::model::{EntityType, MetricType, Sample, SampleType};
use jarvis_lib::{measurement_client::MeasurementClient, model::Measurement};
use log::{debug, info, warn};
use reqwest::Url;
use retry::delay::{jitter, Exponential};
use retry::retry;
use serde_json::Value;
use tungstenite::{connect, Message};
use uuid::Uuid;

use crate::model::{
    Config, TeslaAccessToken, TeslaAccessTokenRequest, TeslaApiResponse, TeslaStreamingApiMessage,
    TeslaVehicle, TeslaVehicleData, TeslaVehicleState, TeslaVehicleStreamingData,
};

const RETRY_INTERVAL_MS: u64 = 100;
const RETRY_FACTOR: f64 = 2.0;
const RETRY_TAKES: usize = 3;

pub struct TeslaApiClient {}

impl MeasurementClient<Config> for TeslaApiClient {
    fn get_measurement(
        &self,
        config: Config,
        last_measurement: Option<Measurement>,
    ) -> Result<Option<Measurement>, Box<dyn Error>> {
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

            let vehicle_data = self.get_streaming_data(&token, &vehicle)?;

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
                let current_charger_power = vehicle_data.charger_power * 1000.0;
                measurement.samples.push(Sample {
                    entity_type: EntityType::Device,
                    entity_name: "jarvis-tesla-exporter".into(),
                    sample_type: SampleType::ElectricityConsumption,
                    sample_name: vehicle.display_name.clone(),
                    metric_type: MetricType::Gauge,
                    value: current_charger_power,
                });

                // store as counter for totals
                let last_charger_power: f64 = if let Some(last_measurement) = last_measurement {
                    last_measurement
                        .samples
                        .iter()
                        .find(|s| {
                            s.entity_type == EntityType::Device
                                && s.sample_type == SampleType::ElectricityConsumption
                                && s.sample_name == vehicle.display_name
                                && s.metric_type == MetricType::Gauge
                        })
                        .map(|s| s.value)
                        .unwrap_or(0.0)
                } else {
                    0.0
                };
                let current_charge_energy_added = if vehicle_data.charger_power > 0.0
                    || last_charger_power > 0.0 && vehicle_data.charger_power == 0.0
                {
                    // get vehicle data through regular api if vehicle is charging or has just finished charging
                    // skip otherwise, because it keeps the vehicle awake
                    let vehicle_data = self.get_vehicle_data(&token, &vehicle)?;

                    vehicle_data.charge_state.charge_energy_added * 1000.0 * 3600.0
                } else {
                    0.0
                };
                measurement.samples.push(Sample {
                    entity_type: EntityType::Device,
                    entity_name: "jarvis-tesla-exporter".into(),
                    sample_type: SampleType::ElectricityConsumption,
                    sample_name: vehicle.display_name,
                    metric_type: MetricType::Counter,
                    value: current_charge_energy_added,
                });

                return Ok(Some(measurement));
            } else {
                info!("Vehicle is not inside geofence, skip storing samples");
                continue;
            }
        }

        info!("No vehicles were inside of any of the configured geofences and awake");
        Ok(None)
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

    #[allow(dead_code)]
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

    pub fn get_streaming_data(
        &self,
        token: &TeslaAccessToken,
        vehicle: &TeslaVehicle,
    ) -> Result<TeslaVehicleStreamingData, Box<dyn Error>> {
        info!(
            "Connecting to streaming api for vehicle {}",
            vehicle.display_name
        );

        let (mut socket, response) =
            connect(Url::parse("wss://streaming.vn.teslamotors.com/streaming/")?)?;

        debug!("Connected to the server");
        debug!("Response HTTP code: {}", response.status());
        debug!("Response contains the following headers:");
        for (ref header, _value) in response.headers() {
            debug!("* {}", header);
        }

        let subscribe_message = TeslaStreamingApiMessage {
            msg_type: "data:subscribe_oauth".into(),
            tag: vehicle.vehicle_id.to_string(),
            token: Some(token.access_token.clone()),
            value: "est_lat,est_lng,power".into(),
        };

        socket.write_message(Message::Text(serde_json::to_string(&subscribe_message)?))?;
        let start = Instant::now();
        loop {
            if start.elapsed().as_secs() > 300 {
                return Err(Box::<dyn Error>::from("Timed out after 300 seconds"));
            };

            let msg = socket.read_message()?;
            debug!("Received: {}", msg);

            if msg.is_close() {
                return Err(Box::<dyn Error>::from("Received close message"));
            }

            if !msg.is_binary() {
                debug!("Message is not of type binary, skipping");
                continue;
            }

            let msg_data = msg.into_data();

            let msg_value: serde_json::Value = serde_json::from_slice(&msg_data)?;
            if let Value::String(msg_type) = &msg_value["msg_type"] {
                match msg_type.as_str() {
                    "data:update" => {
                        let data_update_message: TeslaStreamingApiMessage =
                            serde_json::from_slice(&msg_data)?;

                        if data_update_message.tag != vehicle.vehicle_id.to_string() {
                            warn!("Receiving data for another vehicle");
                            continue;
                        }

                        let values: Vec<String> = data_update_message
                            .value
                            .split(',')
                            .map(str::to_string)
                            .collect();

                        if values.len() != 4 {
                            warn!("Receiving incorrect number of values");
                            continue;
                        }

                        return Ok(TeslaVehicleStreamingData {
                            latitude: values
                                .get(1)
                                .unwrap_or(&"0.0".to_string())
                                .parse()
                                .unwrap_or(0.0),
                            longitude: values
                                .get(2)
                                .unwrap_or(&"0.0".to_string())
                                .parse()
                                .unwrap_or(0.0),
                            charger_power: values
                                .get(3)
                                .unwrap_or(&"0.0".to_string())
                                .parse()
                                .unwrap_or(0.0),
                            charge_energy_added: 0.0,
                        });
                    }
                    "data:error" => {
                        return Err(Box::<dyn Error>::from(format!(
                            "Received error message: {}",
                            &msg_value["error_type"]
                        )));
                    }
                    _ => {
                        debug!("Unhandled message type {}", msg_type);
                    }
                }
            }
        }
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

    #[test]
    #[ignore]
    fn get_streaming_data() {
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
                .get_streaming_data(&token, &vehicle)
                .expect("Failed getting vehicle charge state");

            debug!("{:?}", vehicle_charge_state);
        }
    }
}
