#![warn(clippy::result_large_err)]

use std::env;
use std::fs::{self, File};
use std::io::{Read, Cursor};
use image::imageops::FilterType;
use aws_sdk_s3::Client;
use aws_config::Region;
use aws_credential_types::Credentials;
use async_trait::async_trait;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Помилка файлової системи: {0}")]
    Io(#[from] std::io::Error),
    #[error("Помилка мережі: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Помилка зображення: {0}")]
    Image(#[from] image::ImageError),
    #[error("Помилка парсингу чисел: {0}")]
    ParseInt(#[from] std::num::ParseIntError),
    #[error("Помилка S3: {0}")]
    S3(String),
    #[error("Неправильні аргументи: {0}")]
    Argument(String),
}

#[async_trait]
pub trait Uploader {
    async fn upload(&self, filename: &str, data: &[u8]) -> Result<(), AppError>;
}

pub struct FsUploader {
    base_path: String,
}

#[async_trait]
impl Uploader for FsUploader {
    async fn upload(&self, filename: &str, data: &[u8]) -> Result<(), AppError> {
        let path = format!("{}/{}", self.base_path, filename);
        fs::write(path, data)?;
        Ok(())
    }
}

pub struct S3Uploader {
    client: Client,
    bucket: String,
}

#[async_trait]
impl Uploader for S3Uploader {
    async fn upload(&self, filename: &str, data: &[u8]) -> Result<(), AppError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(filename)
            .body(data.to_vec().into())
            .send()
            .await
            .map_err(|e| AppError::S3(e.to_string()))?;
        Ok(())
    }
}

#[tokio::main]
pub async fn main() -> Result<(), AppError> {
    let args: Vec<String> = env::args().collect();

    let files_index = args.iter().position(|r| r == "--files")
        .ok_or_else(|| AppError::Argument("Відсутній обов'язковий аргумент --files".to_string()))?;
    let resize_index = args.iter().position(|r| r == "--resize")
        .ok_or_else(|| AppError::Argument("Відсутній обов'язковий аргумент --resize".to_string()))?;

    let file_path = args.get(files_index + 1)
        .ok_or_else(|| AppError::Argument("Не вказано шлях до текстового файлу".to_string()))?;
    let resize_str = args.get(resize_index + 1)
        .ok_or_else(|| AppError::Argument("Не вказано необхідний розмір зображень".to_string()))?;

    let dims: Vec<&str> = resize_str.split('x').collect();
    if dims.len() != 2 {
        return Err(AppError::Argument("Неправильний формат розміру, очікується формат WxH".to_string()));
    }

    let width: u32 = dims[0].parse()?;
    let height: u32 = dims[1].parse()?;

    let uploader_type = env::var("MYME_UPLOADER").unwrap_or_else(|_| "fs".to_string());

    let uploader: Box<dyn Uploader> = if uploader_type == "s3" {
        let endpoint = env::var("S3_ENDPOINT").unwrap_or_else(|_| "https://t3.storage.dev".to_string());
        let access_key = env::var("S3_ACCESS_KEY").unwrap_or_else(|_| "".to_string());
        let secret_key = env::var("S3_SECRET_KEY").unwrap_or_else(|_| "".to_string());
        let bucket = env::var("S3_BUCKET").unwrap_or_else(|_| "laba2-syvolap".to_string());

        let credentials = Credentials::new(access_key, secret_key, None, None, "manual");

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(Region::new("auto"))
            .endpoint_url(endpoint)
            .credentials_provider(credentials)
            .load()
            .await;

        let s3_config = aws_sdk_s3::config::Builder::from(&config)
            .force_path_style(true)
            .build();

        Box::new(S3Uploader {
            client: Client::from_conf(s3_config),
            bucket,
        })
    } else {
        let out_dir = env::var("MYME_FILES_PATH").unwrap_or_else(|_| "output".to_string());
        fs::create_dir_all(&out_dir)?;
        Box::new(FsUploader { base_path: out_dir })
    };

    let mut file = File::open(file_path)?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;

    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let img = if line.starts_with("http://") || line.starts_with("https://") {
            let resp = reqwest::get(line).await?.bytes().await?;
            image::load_from_memory(&resp)?
        } else {
            image::open(line)?
        };

        let resized = img.resize_exact(width, height, FilterType::Lanczos3);

        let mut bytes: Vec<u8> = Vec::new();
        let mut cursor = Cursor::new(&mut bytes);
        resized.write_to(&mut cursor, image::ImageOutputFormat::Png)?;

        let out_filename = format!("resized_img_{}.png", i + 1);
        uploader.upload(&out_filename, &bytes).await?;
    }

    Ok(())
}