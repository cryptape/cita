// CITA
// Copyright 2016-2017 Cryptape Technologies LLC.

// This program is free software: you can redistribute it
// and/or modify it under the terms of the GNU General Public
// License as published by the Free Software Foundation,
// either version 3 of the License, or (at your option) any
// later version.

// This program is distributed in the hope that it will be
// useful, but WITHOUT ANY WARRANTY; without even the implied
// warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR
// PURPOSE. See the GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use libproto::*;
use protobuf::Message;
use std::sync::mpsc::{Sender, Receiver};
use std::sync::Arc;
use std::vec::*;
use util::{H256, RwLock};
use verify::Verifier;
use cache::{VerifyCache, VerifyBlockCache, VerifyResult, BlockVerifyStatus};

#[derive(PartialEq)]
pub enum VerifyType {
    SingleVerify,
    BlockVerify,
}


fn verfiy_tx(req: &VerifyTxReq, verifier: &Verifier) -> VerifyTxResp {
    let mut resp = VerifyTxResp::new();
    resp.set_tx_hash(req.get_tx_hash().to_vec());

    if !verifier.verify_valid_until_block(req.get_valid_until_block()) {
        resp.set_ret(Ret::OutOfTime);                            
        return resp;
    }

    let tx_hash = H256::from_slice(req.get_tx_hash());
    let ret = verifier.check_hash_exist(&tx_hash);
    if ret {
        if verifier.is_inited() {
            resp.set_ret(Ret::Dup);
        } else {
            resp.set_ret(Ret::NotReady);
        }                    
        return resp;
    }
    let ret = verifier.verify_sig(req);
    if ret.is_err() {
        resp.set_ret(Ret::BadSig);                            
        return resp;
    }
    //check signer if req have
    let req_signer = req.get_signer();
    if req_signer.len() != 0 {
        if req_signer != ret.unwrap().to_vec().as_slice() {
            resp.set_ret(Ret::BadSig);                            
            return resp;
        }
    }
    resp.set_signer(ret.unwrap().to_vec());
    resp.set_ret(Ret::Ok);
    resp
}

fn get_key(submodule: u32, is_blk: bool) -> String {
    "verify".to_owned() + 
    if is_blk {
        "_blk_"
    } else {
        "_tx_"
    } +
    id_to_key(submodule)
}

pub fn handle_remote_msg(payload: Vec<u8>,
                         verifier: Arc<RwLock<Verifier>>,
                         tx_req: Sender<(VerifyType, u64, VerifyTxReq, u32)>,
                         tx_pub: Sender<(String, Vec<u8>)>,
                         block_cache: Arc<RwLock<VerifyBlockCache>>) {
    let (cmdid, _origin, content) = parse_msg(payload.as_slice());
    let (submodule, _topic) = de_cmd_id(cmdid);
    //let tx_req_block = tx_req.clone();
    match content {
        MsgClass::BLOCKTXHASHES(block_tx_hashes) => {
            let height = block_tx_hashes.get_height();
            trace!("get block tx hashs for height {:?}", height);
            let tx_hashes = block_tx_hashes.get_tx_hashes();
            let mut tx_hashes_in_h256: Vec<H256> = Vec::new();
            for data in tx_hashes.iter() {
                tx_hashes_in_h256.push(H256::from_slice(data));
            }
            verifier.write().update_hashes(height, tx_hashes_in_h256, &tx_pub);
        }
        MsgClass::VERIFYTXREQ(req) => {
            trace!("get verify request {:?}", req);
            tx_req.send((VerifyType::SingleVerify, 0, req, submodule)).unwrap();

        }
        MsgClass::VERIFYBLKREQ(blkreq) => {
            trace!("get block verify request {:?}", blkreq);
            let block_verify_status = BlockVerifyStatus {
                block_verify_result: VerifyResult::VerifyOngoing,
                verify_success_cnt_required: blkreq.get_reqs().len(),
                verify_success_cnt_capture: 0,
            };
            let id = blkreq.get_id();
            block_cache.write().insert(id, block_verify_status);
            for req in blkreq.get_reqs() {
                tx_req.send((VerifyType::BlockVerify, id, req.clone(), submodule)).unwrap();
            }
        }
        _ => {}
    }
}

pub fn handle_verificaton_result(result_receiver: &Receiver<(VerifyType, u64, VerifyTxResp, u32)>,
                                 tx_pub: &Sender<(String, Vec<u8>)>,
                                 block_cache: Arc<RwLock<VerifyBlockCache>>) {
    let (verify_type, id, resp, sub_module) = result_receiver.recv().unwrap();
    match verify_type {
        VerifyType::SingleVerify => {
            trace!("Finish single tx's verification with the response: {:?}", resp);
            let msg = factory::create_msg(submodules::AUTH, topics::VERIFY_TX_RESP, communication::MsgType::VERIFY_TX_RESP, resp.write_to_bytes().unwrap());
            tx_pub.send((get_key(sub_module, false), msg.write_to_bytes().unwrap())).unwrap();
        }
        VerifyType::BlockVerify => {
            if Ret::Ok != resp.get_ret() {
                if let Some(block_verify_status) = block_cache.write().get_mut(id) {
                    block_verify_status.block_verify_result = VerifyResult::VerifyFailed;

                    let mut blkresp = VerifyBlockResp::new();
                    blkresp.set_id(id);
                    blkresp.set_ret(resp.get_ret());

                    let msg = factory::create_msg(submodules::AUTH, topics::VERIFY_BLK_RESP, communication::MsgType::VERIFY_BLK_RESP, blkresp.write_to_bytes().unwrap());
                    trace!("Failed to do verify blk req for block id: {}, ret: {:?}, from: {}", id, blkresp.get_ret(), submodules::CHAIN);
                    tx_pub.send((get_key(sub_module, true), msg.write_to_bytes().unwrap())).unwrap();
                } else {
                    error!("Failed to get block verify status for block height: {:?}", id);
                }
            } else {
                if let Some(block_verify_status) = block_cache.write().get_mut(id) {
                    block_verify_status.verify_success_cnt_capture += 1;
                    if block_verify_status.verify_success_cnt_capture == block_verify_status.verify_success_cnt_required {
                        let mut blkresp = VerifyBlockResp::new();
                        blkresp.set_id(id);
                        blkresp.set_ret(resp.get_ret());

                        let msg = factory::create_msg(submodules::AUTH, topics::VERIFY_BLK_RESP, communication::MsgType::VERIFY_BLK_RESP, blkresp.write_to_bytes().unwrap());
                        trace!("Succeed to do verify blk req for block id: {}, ret: {:?}, from: {}", id, blkresp.get_ret(), submodules::CHAIN);
                        tx_pub.send((get_key(sub_module, true), msg.write_to_bytes().unwrap())).unwrap();
                    }
                } else {
                    error!("Failed to get block verify status for block height: {:?}", id);
                }

            }

        }
    }
}

pub fn verify_tx_service(req: VerifyTxReq, verifier :Arc<RwLock<Verifier>>, cache :Arc<RwLock<VerifyCache>>) -> VerifyTxResp {
    let tx_hash = H256::from_slice(req.get_tx_hash());
    //First,check the tx from the hash
    if let Some(resp) = cache.read().get(&tx_hash) {
        resp.clone()
    } else {
        verfiy_tx(&req, &verifier.read())
    }
}
