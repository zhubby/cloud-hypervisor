// Copyright © 2019 Intel Corporation
//
// SPDX-License-Identifier: Apache-2.0
//

extern crate vmm;

#[macro_use(crate_version, crate_authors)]
extern crate clap;

use clap::{App, Arg};
use std::process;
use vmm::config;

fn main() {
    let cmd_arguments = App::new("cloud-hypervisor")
        .version(crate_version!())
        .author(crate_authors!())
        .about("Launch a cloud-hypervisor VMM.")
        .arg(
            Arg::with_name("cpus")
                .long("cpus")
                .help("Number of virtual CPUs")
                .default_value(config::DEFAULT_VCPUS),
        )
        .arg(
            Arg::with_name("memory")
                .long("memory")
                .help(
                    "Memory parameters \"size=<guest_memory_size>,\
                     file=<backing_file_path>\"",
                )
                .default_value(config::DEFAULT_MEMORY),
        )
        .arg(
            Arg::with_name("kernel")
                .long("kernel")
                .help("Path to kernel image (vmlinux)")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("cmdline")
                .long("cmdline")
                .help("Kernel command line")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("disk")
                .long("disk")
                .help("Path to VM disk image")
                .takes_value(true)
                .min_values(1),
        )
        .arg(
            Arg::with_name("net")
                .long("net")
                .help(
                    "Network parameters \"tap=<if_name>,\
                     ip=<ip_addr>,mask=<net_mask>,mac=<mac_addr>\"",
                )
                .takes_value(true)
                .min_values(1),
        )
        .arg(
            Arg::with_name("rng")
                .long("rng")
                .help("Path to entropy source")
                .default_value(config::DEFAULT_RNG_SOURCE),
        )
        .arg(
            Arg::with_name("fs")
                .long("fs")
                .help(
                    "virtio-fs parameters \"tag=<tag_name>,\
                     sock=<socket_path>,num_queues=<number_of_queues>,\
                     queue_size=<size_of_each_queue>\"",
                )
                .takes_value(true)
                .min_values(1),
        )
        .arg(
            Arg::with_name("pmem")
                .long("pmem")
                .help(
                    "Persistent memory parameters \"file=<backing_file_path>,\
                     size=<persistent_memory_size>\"",
                )
                .takes_value(true)
                .min_values(1),
        )
        .arg(
            Arg::with_name("serial")
                .long("serial")
                .help("Control serial port: off|tty|file=/path/to/a/file")
                .default_value("tty"),
        )
        .get_matches();

    // These .unwrap()s cannot fail as there is a default value defined
    let cpus = cmd_arguments.value_of("cpus").unwrap();
    let memory = cmd_arguments.value_of("memory").unwrap();
    let rng = cmd_arguments.value_of("rng").unwrap();
    let serial = cmd_arguments.value_of("serial").unwrap();

    let kernel = cmd_arguments
        .value_of("kernel")
        .expect("Missing argument: kernel");
    let cmdline = cmd_arguments.value_of("cmdline");

    let disks: Option<Vec<&str>> = cmd_arguments.values_of("disk").map(|x| x.collect());
    let net: Option<Vec<&str>> = cmd_arguments.values_of("net").map(|x| x.collect());
    let fs: Option<Vec<&str>> = cmd_arguments.values_of("fs").map(|x| x.collect());
    let pmem: Option<Vec<&str>> = cmd_arguments.values_of("pmem").map(|x| x.collect());

    let vm_config = match config::VmConfig::parse(config::VmParams {
        cpus,
        memory,
        kernel,
        cmdline,
        disks,
        net,
        rng,
        fs,
        pmem,
        serial,
    }) {
        Ok(config) => config,
        Err(e) => {
            println!("Failed parsing parameters {:?}", e);
            process::exit(1);
        }
    };

    println!(
        "Cloud Hypervisor Guest\n\tvCPUs: {}\n\tMemory: {} MB\
         \n\tKernel: {:?}\n\tKernel cmdline: {}\n\tDisk(s): {:?}",
        u8::from(&vm_config.cpus),
        vm_config.memory.size >> 20,
        vm_config.kernel.path,
        vm_config.cmdline.args.as_str(),
        vm_config.disks,
    );

    if let Err(e) = vmm::boot_kernel(vm_config) {
        println!("Guest boot failed: {}", e);
        process::exit(1);
    }
}

#[cfg(test)]
#[cfg(feature = "integration_tests")]
#[macro_use]
extern crate credibility;

#[cfg(test)]
#[cfg(feature = "integration_tests")]
mod tests {
    use ssh2::Session;
    use std::fs::{self, read, OpenOptions};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::process::Command;
    use std::string::String;
    use std::thread;
    use tempdir::TempDir;

    struct Guest {
        tmp_dir: TempDir,
        disks: Vec<String>,
        fw_path: String,
        guest_ip: String,
        host_ip: String,
        guest_mac: String,
    }

    fn prepare_virtiofsd(tmp_dir: &TempDir) -> (std::process::Child, String) {
        let mut workload_path = dirs::home_dir().unwrap();
        workload_path.push("workloads");

        let mut virtiofsd_path = workload_path.clone();
        virtiofsd_path.push("virtiofsd");
        let virtiofsd_path = String::from(virtiofsd_path.to_str().unwrap());

        let mut shared_dir_path = workload_path.clone();
        shared_dir_path.push("shared_dir");
        let shared_dir_path = String::from(shared_dir_path.to_str().unwrap());

        let virtiofsd_socket_path =
            String::from(tmp_dir.path().join("virtiofs.sock").to_str().unwrap());

        // Start the daemon
        let child = Command::new(virtiofsd_path.as_str())
            .args(&[
                "-o",
                format!("vhost_user_socket={}", virtiofsd_socket_path).as_str(),
            ])
            .args(&["-o", format!("source={}", shared_dir_path).as_str()])
            .args(&["-o", "cache=none"])
            .spawn()
            .unwrap();

        (child, virtiofsd_socket_path)
    }

    impl Guest {
        fn new() -> Self {
            let tmp_dir = TempDir::new("ch").unwrap();

            let mut guest = Guest {
                tmp_dir,
                disks: Vec::new(),
                fw_path: String::new(),
                guest_ip: String::from("192.168.2.2"),
                host_ip: String::from("192.168.2.1"),
                guest_mac: String::from("12:34:56:78:90:ab"),
            };

            guest.prepare_files();

            guest
        }

        fn prepare_cloudinit(&self) -> String {
            let cloudinit_file_path =
                String::from(self.tmp_dir.path().join("cloudinit").to_str().unwrap());

            let cloud_init_directory = self.tmp_dir.path().join("cloud-init").join("openstack");

            fs::create_dir_all(&cloud_init_directory.join("latest"))
                .expect("Expect creating cloud-init directory to succeed");

            let source_file_dir = std::env::current_dir()
                .unwrap()
                .join("test_data")
                .join("cloud-init")
                .join("openstack")
                .join("latest");

            fs::copy(
                source_file_dir.join("meta_data.json"),
                cloud_init_directory.join("latest").join("meta_data.json"),
            )
            .expect("Expect copying cloud-init meta_data.json to succeed");

            let mut user_data_string = String::new();

            fs::File::open(source_file_dir.join("user_data"))
                .unwrap()
                .read_to_string(&mut user_data_string)
                .expect("Expected reading user_data file in to succeed");

            user_data_string = user_data_string.replace("192.168.2.1", &self.host_ip);
            user_data_string = user_data_string.replace("192.168.2.2", &self.guest_ip);
            user_data_string = user_data_string.replace("12:34:56:78:90:ab", &self.guest_mac);

            fs::File::create(cloud_init_directory.join("latest").join("user_data"))
                .unwrap()
                .write_all(&user_data_string.as_bytes())
                .expect("Expected writing out user_data to succeed");

            std::process::Command::new("mkdosfs")
                .args(&["-n", "config-2"])
                .args(&["-C", cloudinit_file_path.as_str()])
                .arg("8192")
                .output()
                .expect("Expect creating disk image to succeed");

            std::process::Command::new("mcopy")
                .arg("-o")
                .args(&["-i", cloudinit_file_path.as_str()])
                .args(&["-s", cloud_init_directory.to_str().unwrap(), "::"])
                .output()
                .expect("Expect copying files to disk image to succeed");

            cloudinit_file_path
        }

        fn prepare_files(&mut self) {
            let mut workload_path = dirs::home_dir().unwrap();
            workload_path.push("workloads");

            let mut fw_path = workload_path.clone();
            fw_path.push("hypervisor-fw");

            let mut osdisk_base_path = workload_path.clone();
            osdisk_base_path.push("clear-29810-cloud.img");

            let mut osdisk_raw_base_path = workload_path.clone();
            osdisk_raw_base_path.push("clear-29810-cloud-raw.img");

            let osdisk_path =
                String::from(self.tmp_dir.path().join("osdisk.img").to_str().unwrap());
            let osdisk_raw_path =
                String::from(self.tmp_dir.path().join("osdisk_raw.img").to_str().unwrap());
            let cloudinit_path = self.prepare_cloudinit();

            fs::copy(osdisk_base_path, &osdisk_path)
                .expect("copying of OS source disk image failed");
            fs::copy(osdisk_raw_base_path, &osdisk_raw_path)
                .expect("copying of OS source disk raw image failed");

            let disks = vec![osdisk_path, cloudinit_path, osdisk_raw_path];

            self.disks = disks;
            self.fw_path = String::from(fw_path.to_str().unwrap());
        }

        fn default_net_string(&self) -> String {
            format!(
                "tap=,mac={},ip={},mask=255.255.255.0",
                self.guest_mac, self.host_ip
            )
        }

        fn ssh_command(&self, command: &str) -> String {
            let mut s = String::new();
            #[derive(Debug)]
            enum Error {
                Connection,
                Authentication,
                Command,
            };

            let mut counter = 0;
            loop {
                match (|| -> Result<(), Error> {
                    let tcp = TcpStream::connect(format!("{}:22", self.guest_ip))
                        .map_err(|_| Error::Connection)?;
                    let mut sess = Session::new().unwrap();
                    sess.handshake(&tcp).map_err(|_| Error::Connection)?;

                    sess.userauth_password("admin", "cloud123")
                        .map_err(|_| Error::Authentication)?;
                    assert!(sess.authenticated());

                    let mut channel = sess.channel_session().map_err(|_| Error::Command)?;
                    channel.exec(command).map_err(|_| Error::Command)?;

                    // Intentionally ignore these results here as their failure
                    // does not precipitate a repeat
                    let _ = channel.read_to_string(&mut s);
                    let _ = channel.close();
                    let _ = channel.wait_close();
                    Ok(())
                })() {
                    Ok(_) => break,
                    Err(e) => {
                        counter += 1;
                        if counter >= 6 {
                            panic!("Took too many attempts to run command. Last error: {:?}", e);
                        }
                    }
                };
                thread::sleep(std::time::Duration::new(10 * counter, 0));
            }
            s
        }

        fn get_cpu_count(&self) -> u32 {
            self.ssh_command("grep -c processor /proc/cpuinfo")
                .trim()
                .parse()
                .unwrap()
        }

        fn get_initial_apicid(&self) -> u32 {
            self.ssh_command("grep \"initial apicid\" /proc/cpuinfo | grep -o \"[0-9]*\"")
                .trim()
                .parse()
                .unwrap()
        }

        fn get_total_memory(&self) -> u32 {
            self.ssh_command("grep MemTotal /proc/meminfo | grep -o \"[0-9]*\"")
                .trim()
                .parse::<u32>()
                .unwrap()
        }

        fn get_entropy(&self) -> u32 {
            self.ssh_command("cat /proc/sys/kernel/random/entropy_avail")
                .trim()
                .parse::<u32>()
                .unwrap()
        }

        fn get_pci_bridge_class(&self) -> String {
            self.ssh_command("cat /sys/bus/pci/devices/0000:00:00.0/class")
                .trim()
                .to_string()
        }
    }

    #[test]
    fn test_simple_launch() {
        test_block!(tb, "", {
            let guest = Guest::new();

            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", guest.fw_path.as_str()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            aver_eq!(tb, guest.get_cpu_count(), 1);
            aver_eq!(tb, guest.get_initial_apicid(), 0);
            aver!(tb, guest.get_total_memory() > 496_000);
            aver!(tb, guest.get_entropy() >= 1000);
            aver_eq!(tb, guest.get_pci_bridge_class(), "0x060000");

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_multi_cpu() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "2"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", guest.fw_path.as_str()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            aver_eq!(tb, guest.get_cpu_count(), 2);

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_large_memory() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=5120M"])
                .args(&["--kernel", guest.fw_path.as_str()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            aver!(tb, guest.get_total_memory() > 5_063_000);

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_pci_msi() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", guest.fw_path.as_str()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            aver_eq!(
                tb,
                guest
                    .ssh_command("grep -c PCI-MSI /proc/interrupts")
                    .trim()
                    .parse::<u32>()
                    .unwrap(),
                8
            );

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_vmlinux_boot() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut workload_path = dirs::home_dir().unwrap();
            workload_path.push("workloads");

            let mut kernel_path = workload_path.clone();
            kernel_path.push("vmlinux");

            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", kernel_path.to_str().unwrap()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .args(&["--cmdline", "root=PARTUUID=3cb0e0a5-925d-405e-bc55-edf0cec8f10a console=tty0 console=ttyS0,115200n8 console=hvc0 quiet init=/usr/lib/systemd/systemd-bootchart initcall_debug tsc=reliable no_timer_check noreplace-smp cryptomgr.notests rootfstype=ext4,btrfs,xfs kvm-intel.nested=1 rw"])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            aver_eq!(tb, guest.get_cpu_count(), 1);
            aver!(tb, guest.get_total_memory() > 496_000);
            aver!(tb, guest.get_entropy() >= 1000);
            aver_eq!(
                tb,
                guest
                    .ssh_command("grep -c PCI-MSI /proc/interrupts")
                    .trim()
                    .parse::<u32>()
                    .unwrap(),
                8
            );

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_bzimage_boot() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut workload_path = dirs::home_dir().unwrap();
            workload_path.push("workloads");

            let mut kernel_path = workload_path.clone();
            kernel_path.push("bzImage");

            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", kernel_path.to_str().unwrap()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .args(&["--cmdline", "root=PARTUUID=3cb0e0a5-925d-405e-bc55-edf0cec8f10a console=tty0 console=ttyS0,115200n8 console=hvc0 quiet init=/usr/lib/systemd/systemd-bootchart initcall_debug tsc=reliable no_timer_check noreplace-smp cryptomgr.notests rootfstype=ext4,btrfs,xfs kvm-intel.nested=1 rw"])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            aver_eq!(tb, guest.get_cpu_count(), 1);
            aver!(tb, guest.get_total_memory() > 496_000);
            aver!(tb, guest.get_entropy() >= 1000);
            aver_eq!(
                tb,
                guest
                    .ssh_command("grep -c PCI-MSI /proc/interrupts")
                    .trim()
                    .parse::<u32>()
                    .unwrap(),
                8
            );

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_split_irqchip() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", guest.fw_path.as_str()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            aver_eq!(
                tb,
                guest
                    .ssh_command("cat /proc/interrupts | grep 'IO-APIC' | grep -c 'timer'")
                    .trim()
                    .parse::<u32>()
                    .unwrap(),
                0
            );
            aver_eq!(
                tb,
                guest
                    .ssh_command("cat /proc/interrupts | grep 'IO-APIC' | grep -c 'cascade'")
                    .trim()
                    .parse::<u32>()
                    .unwrap(),
                0
            );

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_virtio_fs() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let (mut daemon_child, virtiofsd_socket_path) = prepare_virtiofsd(&guest.tmp_dir);
            let mut workload_path = dirs::home_dir().unwrap();
            workload_path.push("workloads");

            let mut kernel_path = workload_path.clone();
            kernel_path.push("vmlinux-custom");

            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M,file=/dev/shm"])
                .args(&["--kernel", kernel_path.to_str().unwrap()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .args(&[
                    "--fs",
                    format!(
                        "tag=virtiofs,sock={},num_queues=1,queue_size=1024",
                        virtiofsd_socket_path
                    )
                    .as_str(),
                ])
                .args(&["--cmdline", "root=PARTUUID=3cb0e0a5-925d-405e-bc55-edf0cec8f10a console=tty0 console=ttyS0,115200n8 console=hvc0 quiet init=/usr/lib/systemd/systemd-bootchart initcall_debug tsc=reliable no_timer_check noreplace-smp cryptomgr.notests rootfstype=ext4,btrfs,xfs kvm-intel.nested=1 rw"])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            // Mount shared directory through virtio_fs filesystem
            aver_eq!(
                tb,
                guest.ssh_command("mkdir -p mount_dir && sudo mount -t virtio_fs /dev/null mount_dir/ -o tag=virtiofs,rootmode=040000,user_id=1001,group_id=1001 && echo ok")
                    .trim(),
                "ok"
            );
            // Check file1 exists and its content is "foo"
            aver_eq!(tb, guest.ssh_command("cat mount_dir/file1").trim(), "foo");
            // Check file2 does not exist
            aver_ne!(
                tb,
                guest.ssh_command("ls mount_dir/file2").trim(),
                "mount_dir/file2"
            );
            // Check file3 exists and its content is "bar"
            aver_eq!(tb, guest.ssh_command("cat mount_dir/file3").trim(), "bar");

            guest.ssh_command("sudo reboot");
            let _ = child.wait();
            let _ = daemon_child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_virtio_pmem() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut workload_path = dirs::home_dir().unwrap();
            workload_path.push("workloads");

            let mut kernel_path = workload_path.clone();
            kernel_path.push("vmlinux-custom");

            let pmem_backend_path = guest.tmp_dir.path().join("/tmp/pmem-file");
            let mut pmem_backend_file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(&pmem_backend_path)
                .unwrap();

            let pmem_backend_content = "foo";
            pmem_backend_file
                .write_all(pmem_backend_content.as_bytes())
                .unwrap();
            let pmem_backend_file_size = 0x1000;
            pmem_backend_file.set_len(pmem_backend_file_size).unwrap();

            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", kernel_path.to_str().unwrap()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .args(&[
                    "--pmem",
                    format!(
                        "file={},size={}",
                        pmem_backend_path.to_str().unwrap(),
                        pmem_backend_file_size
                    )
                    .as_str(),
                ])
                .args(&["--cmdline", "root=PARTUUID=3cb0e0a5-925d-405e-bc55-edf0cec8f10a console=tty0 console=ttyS0,115200n8 console=hvc0 quiet init=/usr/lib/systemd/systemd-bootchart initcall_debug tsc=reliable no_timer_check noreplace-smp cryptomgr.notests rootfstype=ext4,btrfs,xfs kvm-intel.nested=1 rw"])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            // Check for the presence of /dev/pmem0
            aver_eq!(tb, guest.ssh_command("ls /dev/pmem0").trim(), "/dev/pmem0");
            // Check content
            aver_eq!(
                tb,
                &guest.ssh_command("sudo cat /dev/pmem0").trim()[..pmem_backend_content.len()],
                pmem_backend_content
            );
            // Modify content
            let new_content = "bar";
            guest.ssh_command(
                format!(
                    "sudo bash -c 'echo {} > /dev/pmem0' && sudo sync /dev/pmem0",
                    new_content
                )
                .as_str(),
            );

            // Check content from host
            aver_eq!(
                tb,
                &String::from_utf8(read(pmem_backend_path).unwrap())
                    .unwrap()
                    .as_str()[..new_content.len()],
                new_content
            );

            guest.ssh_command("sudo reboot");
            let _ = child.wait();

            Ok(())
        });
    }

    #[test]
    fn test_boot_from_virtio_pmem() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut workload_path = dirs::home_dir().unwrap();
            workload_path.push("workloads");

            let mut kernel_path = workload_path.clone();
            kernel_path.push("vmlinux-custom");

            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", kernel_path.to_str().unwrap()])
                .args(&["--disk", guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .args(&[
                    "--pmem",
                    format!(
                        "file={},size={}",
                        guest.disks[2],
                        fs::metadata(&guest.disks[2]).unwrap().len()
                    )
                    .as_str(),
                ])
                .args(&["--cmdline", "root=PARTUUID=3cb0e0a5-925d-405e-bc55-edf0cec8f10a console=tty0 console=ttyS0,115200n8 console=hvc0 quiet init=/usr/lib/systemd/systemd-bootchart initcall_debug tsc=reliable no_timer_check noreplace-smp cryptomgr.notests rootfstype=ext4,btrfs,xfs kvm-intel.nested=1 rw"])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            // Simple checks to validate the VM booted properly
            aver_eq!(tb, guest.get_cpu_count(), 1);
            aver!(tb, guest.get_total_memory() > 496_000);

            guest.ssh_command("sudo reboot");
            let _ = child.wait();

            Ok(())
        });
    }

    #[test]
    fn test_multiple_network_interfaces() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", guest.fw_path.as_str()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&[
                    "--net",
                    guest.default_net_string().as_str(),
                    "tap=,mac=8a:6b:6f:5a:de:ac,ip=192.168.3.1,mask=255.255.255.0",
                    "tap=,mac=fe:1f:9e:e1:60:f2,ip=192.168.4.1,mask=255.255.255.0",
                ])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            // 3 network interfaces + default localhost ==> 4 interfaces
            aver_eq!(
                tb,
                guest
                    .ssh_command("ip -o link | wc -l")
                    .trim()
                    .parse::<u32>()
                    .unwrap(),
                4
            );

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_serial_disable() {
        test_block!(tb, "", {
            let guest = Guest::new();
            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", guest.fw_path.as_str()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .args(&["--serial", "off"])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            // Test that there is no ttyS0
            aver_eq!(
                tb,
                guest
                    .ssh_command("cat /proc/interrupts | grep 'IO-APIC' | grep -c 'ttyS0'")
                    .trim()
                    .parse::<u32>()
                    .unwrap(),
                0
            );

            // Further test that we're MSI only now
            aver_eq!(
                tb,
                guest
                    .ssh_command("cat /proc/interrupts | grep -c 'IO-APIC'")
                    .trim()
                    .parse::<u32>()
                    .unwrap(),
                0
            );

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        });
    }

    #[test]
    fn test_serial_file() {
        test_block!(tb, "", {
            let guest = Guest::new();

            let serial_path = guest.tmp_dir.path().join("/tmp/serial-output");
            let mut child = Command::new("target/debug/cloud-hypervisor")
                .args(&["--cpus", "1"])
                .args(&["--memory", "size=512M"])
                .args(&["--kernel", guest.fw_path.as_str()])
                .args(&["--disk", guest.disks[0].as_str(), guest.disks[1].as_str()])
                .args(&["--net", guest.default_net_string().as_str()])
                .args(&[
                    "--serial",
                    format!("file={}", serial_path.to_str().unwrap()).as_str(),
                ])
                .spawn()
                .unwrap();

            thread::sleep(std::time::Duration::new(20, 0));

            // Test that there is a ttyS0
            aver_eq!(
                tb,
                guest
                    .ssh_command("cat /proc/interrupts | grep 'IO-APIC' | grep -c 'ttyS0'")
                    .trim()
                    .parse::<u32>()
                    .unwrap(),
                1
            );

            guest.ssh_command("sudo reboot");
            thread::sleep(std::time::Duration::new(10, 0));

            // Do this check after shutdown of the VM as an easy way to ensure
            // all writes are flushed to disk
            let mut f = std::fs::File::open(serial_path).unwrap();
            let mut buf = String::new();
            f.read_to_string(&mut buf).unwrap();
            aver!(tb, buf.contains("cloud login:"));

            let _ = child.kill();
            let _ = child.wait();

            Ok(())
        });
    }
}
